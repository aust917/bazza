//! Explicit private template persistence. Ordinary native startup remains v9.
//! Wire mirror stays in sync with service-api/templates.rs; no API dependency.
include!("templates_types.rs");

use super::{bounded_text, check_revision, consistency, revision, DataError, DataResult};
use crate::Store;
use chrono::Duration;
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};

const TOTAL_RESERVED_BYTES: u64 = 64 * 1024 * 1024;

fn nonnegative(value: i64) -> DataResult<u64> {
    u64::try_from(value).map_err(|_| DataError::Invalid("negative stored template count"))
}
fn sql_integer(value: u64) -> DataResult<i64> {
    i64::try_from(value).map_err(|_| DataError::Invalid("template SQL integer range"))
}
fn sha(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn fingerprint<T: Serialize>(value: &T) -> DataResult<String> {
    if !template_body_fits(value, MAX_TEMPLATE_DATA_BYTES) {
        return Err(DataError::Invalid("template request budget"));
    }
    Ok(sha(&serde_json::to_vec(value)?))
}
fn scope_on(conn: &Connection) -> DataResult<String> {
    let value: String = conn.query_row(
        "SELECT cursor_scope FROM service_search_state WHERE singleton=1",
        [],
        |r| r.get(0),
    )?;
    if !scope(&value) {
        return Err(DataError::Invalid("template library scope"));
    }
    Ok(value)
}
fn catalog_on(conn: &Connection) -> DataResult<String> {
    let value: String = conn.query_row(
        "SELECT revision FROM service_template_catalog WHERE singleton=1",
        [],
        |r| r.get(0),
    )?;
    if !token(&value) {
        return Err(DataError::Invalid("template catalog revision"));
    }
    Ok(value)
}
fn bump_catalog(tx: &Transaction<'_>) -> DataResult<String> {
    let value = revision();
    tx.execute(
        "UPDATE service_template_catalog SET revision=?1 WHERE singleton=1",
        [&value],
    )?;
    Ok(value)
}
fn prefix(bytes: Vec<u8>, max: usize) -> DataResult<String> {
    let valid = match std::str::from_utf8(&bytes) {
        Ok(s) => s,
        Err(e) if e.error_len().is_none() => std::str::from_utf8(&bytes[..e.valid_up_to()])
            .map_err(|_| DataError::Invalid("template UTF-8"))?,
        Err(_) => return Err(DataError::Invalid("template UTF-8")),
    };
    let mut end = valid.len().min(max);
    while !valid.is_char_boundary(end) {
        end -= 1;
    }
    Ok(valid[..end].to_owned())
}
fn header_on(conn: &Connection, id: &str) -> DataResult<TemplateHeader> {
    if !template_id(id) {
        return Err(DataError::Invalid("template identifier"));
    }
    let saved: Option<(String, String, String, bool)> = conn
        .query_row(
            "SELECT revision,generation,state,builtin FROM service_template_state WHERE id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM templates WHERE id=?1)",
        [id],
        |r| r.get(0),
    )?;
    let Some((rev, generation, state, builtin)) = saved else {
        if exists {
            return Err(DataError::IndexUnavailable);
        }
        let absent = fingerprint(&("template_absent", scope_on(conn)?, id))?;
        return Ok(TemplateHeader {
            id: id.into(),
            revision: absent.clone(),
            generation: absent,
            state: TemplateDocumentState::Absent,
            builtin: false,
            name_preview: String::new(),
            description_preview: String::new(),
            name_bytes: 0,
            description_bytes: 0,
            body_bytes: 0,
        });
    };
    if state == "deleted" {
        if exists {
            return Err(DataError::IndexUnavailable);
        }
        return Ok(TemplateHeader {
            id: id.into(),
            revision: rev,
            generation,
            state: TemplateDocumentState::Deleted,
            builtin,
            name_preview: String::new(),
            description_preview: String::new(),
            name_bytes: 0,
            description_bytes: 0,
            body_bytes: 0,
        });
    }
    if state != "present" || !exists {
        return Err(DataError::IndexUnavailable);
    }
    let (actual_builtin,name_bytes,description_bytes,body_bytes,name,description):(bool,i64,i64,i64,Vec<u8>,Vec<u8>)=conn.query_row(
        "SELECT builtin!=0,length(CAST(name AS BLOB)),length(CAST(description AS BLOB)),length(CAST(body AS BLOB)),COALESCE(substr(CAST(name AS BLOB),1,259),X''),COALESCE(substr(CAST(description AS BLOB),1,515),X'') FROM templates WHERE id=?1",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?)))?;
    if actual_builtin != builtin {
        return Err(DataError::IndexUnavailable);
    }
    let value = TemplateHeader {
        id: id.into(),
        revision: rev,
        generation,
        state: TemplateDocumentState::Present,
        builtin,
        name_preview: prefix(name, 256)?,
        description_preview: prefix(description, 512)?,
        name_bytes: nonnegative(name_bytes)?,
        description_bytes: nonnegative(description_bytes)?,
        body_bytes: nonnegative(body_bytes)?,
    };
    if !value.valid() {
        return Err(DataError::Invalid("template header projection"));
    }
    Ok(value)
}
fn list_on(conn: &Connection, r: &TemplateListRequest) -> DataResult<DesktopTemplatesReply> {
    let library_scope = scope_on(conn)?;
    let catalog_revision = catalog_on(conn)?;
    let query_hash = fingerprint(&("template_list_v1", r.limit, r.max_bytes))?;
    let offset = if let Some(c) = &r.cursor {
        if c.library_scope != library_scope
            || c.catalog_revision != catalog_revision
            || c.query_hash != query_hash
        {
            return Err(DataError::StaleCursor);
        }
        c.offset
    } else {
        0
    };
    let mut statement=conn.prepare("SELECT CASE WHEN length(CAST(t.id AS BLOB)) BETWEEN 1 AND 512 AND instr(CAST(t.id AS BLOB),X'00')=0 THEN t.id END FROM templates t ORDER BY t.builtin DESC,t.name,t.id LIMIT ?1 OFFSET ?2")?;
    let ids = statement
        .query_map(params![r.limit as u32 + 1, sql_integer(offset)?], |row| {
            row.get::<_, Option<String>>(0)
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut page = TemplateListPage {
        library_scope,
        catalog_revision,
        items: Vec::new(),
        next: None,
    };
    for (index, id) in ids.iter().take(r.limit as usize).enumerate() {
        let header = id
            .as_deref()
            .ok_or(DataError::Invalid("legacy template identifier budget"))
            .and_then(|id| header_on(conn, id));
        let header = match header {
            Ok(header) => header,
            Err(DataError::Invalid(_)) if !page.items.is_empty() => {
                page.next = Some(TemplateListCursor {
                    library_scope: page.library_scope.clone(),
                    catalog_revision: page.catalog_revision.clone(),
                    query_hash: query_hash.clone(),
                    offset: offset + page.items.len() as u64,
                });
                break;
            }
            Err(error) => return Err(error),
        };
        page.items.push(header);
        page.next = (index + 1 < ids.len()).then(|| TemplateListCursor {
            library_scope: page.library_scope.clone(),
            catalog_revision: page.catalog_revision.clone(),
            query_hash: query_hash.clone(),
            offset: offset + page.items.len() as u64,
        });
        if !template_body_fits(
            &DesktopTemplatesReply::List(page.clone()),
            r.max_bytes as usize,
        ) {
            page.items.pop();
            if page.items.is_empty() {
                return Err(DataError::Invalid("template header page budget"));
            }
            page.next = Some(TemplateListCursor {
                library_scope: page.library_scope.clone(),
                catalog_revision: page.catalog_revision.clone(),
                query_hash: query_hash.clone(),
                offset: offset + page.items.len() as u64,
            });
            break;
        }
    }
    Ok(DesktopTemplatesReply::List(page))
}
fn field_on(conn: &Connection, r: &TemplateReadRequest) -> DataResult<DesktopTemplatesReply> {
    let library_scope = scope_on(conn)?;
    let header = header_on(conn, &r.template_id)?;
    check_revision(&header.revision, &r.expected_revision)?;
    if header.state != TemplateDocumentState::Present {
        return Err(DataError::NotFound);
    }
    let offset = if let Some(c) = &r.cursor {
        if c.library_scope != library_scope || c.generation != header.generation {
            return Err(DataError::StaleCursor);
        }
        c.byte_offset
    } else {
        0
    };
    let total = match r.field {
        TemplateField::Name => header.name_bytes,
        TemplateField::Description => header.description_bytes,
        TemplateField::Body => header.body_bytes,
    };
    if offset > total {
        return Err(DataError::Invalid("template field offset"));
    }
    let column = match r.field {
        TemplateField::Name => "name",
        TemplateField::Description => "description",
        TemplateField::Body => "body",
    };
    let bytes: Vec<u8> = conn.query_row(
        &format!(
            "SELECT COALESCE(substr(CAST({column} AS BLOB),?2,?3),X'') FROM templates WHERE id=?1"
        ),
        params![
            r.template_id,
            sql_integer(offset + 1)?,
            sql_integer((total - offset).min(r.max_bytes as u64 + 3))?
        ],
        |row| row.get(0),
    )?;
    let text = prefix(bytes, r.max_bytes as usize)?;
    let mut end = text.len();
    loop {
        let next_offset = offset + end as u64;
        let page = TemplateFieldPage {
            library_scope: library_scope.clone(),
            header: header.clone(),
            field: r.field,
            text: text[..end].into(),
            byte_offset: offset,
            total_bytes: total,
            next: (next_offset < total).then(|| TemplateReadCursor {
                library_scope: library_scope.clone(),
                template_id: r.template_id.clone(),
                revision: header.revision.clone(),
                generation: header.generation.clone(),
                field: r.field,
                byte_offset: next_offset,
                max_bytes: r.max_bytes,
            }),
        };
        let reply = DesktopTemplatesReply::ReadField(page);
        if template_body_fits(&reply, r.max_bytes as usize) && (end > 0 || offset == total) {
            return Ok(reply);
        }
        if end == 0 {
            return Err(DataError::Invalid("template field page budget"));
        }
        end /= 2;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
    }
}

struct Upload {
    public: TemplateUploadState,
    principal: String,
    intent_hash: String,
    target_revision: String,
    target_generation: String,
    name: Option<String>,
    description: Option<String>,
    sha256: String,
}
fn upload_on(conn: &Connection, id: Uuid, principal: &str) -> DataResult<Upload> {
    let row=conn.query_row("SELECT principal,intent_hash,template_id,target_revision,target_generation,CASE WHEN length(CAST(name AS BLOB))<=512 THEN name END,CASE WHEN length(CAST(description AS BLOB))<=8192 THEN description END,total_bytes,sha256,received_bytes,next_sequence,state,expires_at,CASE WHEN length(CAST(receipt_json AS BLOB))<=65024 THEN receipt_json END FROM service_template_uploads WHERE id=?1",[id.to_string()],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,Option<String>>(5)?,r.get::<_,Option<String>>(6)?,r.get::<_,i64>(7)?,r.get::<_,String>(8)?,r.get::<_,i64>(9)?,r.get::<_,u32>(10)?,r.get::<_,String>(11)?,r.get::<_,DateTime<Utc>>(12)?,r.get::<_,Option<String>>(13)?))).optional()?.ok_or(DataError::NotFound)?;
    if row.0 != principal {
        return Err(DataError::Forbidden);
    }
    let status = match row.11.as_str() {
        "open" => TemplateUploadStatus::Open,
        "committed" => TemplateUploadStatus::Committed,
        "aborted" => TemplateUploadStatus::Aborted,
        "expired" => TemplateUploadStatus::Expired,
        _ => return Err(DataError::Invalid("stored template upload")),
    };
    let receipt = row.13.as_deref().map(serde_json::from_str).transpose()?;
    let value = Upload {
        public: TemplateUploadState {
            upload_id: id,
            template_id: row.2,
            status,
            next_sequence: row.10,
            received_bytes: nonnegative(row.9)?,
            total_bytes: nonnegative(row.7)?,
            expires_at: row.12,
            receipt,
        },
        principal: row.0,
        intent_hash: row.1,
        target_revision: row.3,
        target_generation: row.4,
        name: row.5,
        description: row.6,
        sha256: row.8,
    };
    if !value.public.valid() {
        return Err(DataError::Invalid("stored template upload"));
    }
    Ok(value)
}
fn retire_on(tx: &Transaction<'_>, id: Uuid, state: &str) -> DataResult<()> {
    tx.execute(
        "DELETE FROM service_template_chunks WHERE upload_id=?1",
        [id.to_string()],
    )?;
    tx.execute("UPDATE service_template_uploads SET state=?2,name=NULL,description=NULL WHERE id=?1 AND state='open'",params![id.to_string(),state])?;
    Ok(())
}
fn expire_on(tx: &Transaction<'_>, mut upload: Upload) -> DataResult<Upload> {
    if upload.public.status == TemplateUploadStatus::Open && upload.public.expires_at <= Utc::now()
    {
        retire_on(tx, upload.public.upload_id, "expired")?;
        upload = upload_on(tx, upload.public.upload_id, &upload.principal)?;
    }
    Ok(upload)
}
fn expire_pending_on(tx: &Transaction<'_>, limit: u16) -> DataResult<u32> {
    let ids = {
        let mut st=tx.prepare("SELECT id FROM service_template_uploads WHERE state='open' AND expires_at<=?1 ORDER BY expires_at,id LIMIT ?2")?;
        let rows = st
            .query_map(params![Utc::now(), limit], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    for id in &ids {
        retire_on(
            tx,
            Uuid::parse_str(id)
                .map_err(|_| DataError::Invalid("stored template upload identity"))?,
            "expired",
        )?;
    }
    Ok(ids.len() as u32)
}
fn begin_on(
    tx: &Transaction<'_>,
    principal: &str,
    r: &TemplateBeginWrite,
) -> DataResult<DesktopTemplatesReply> {
    let intent = fingerprint(r)?;
    match upload_on(tx, r.upload_id, principal) {
        Ok(saved) => {
            if saved.intent_hash != intent {
                return Err(DataError::IdempotencyConflict);
            }
            return Ok(DesktopTemplatesReply::Upload(expire_on(tx, saved)?.public));
        }
        Err(DataError::NotFound) => {}
        Err(e) => return Err(e),
    }
    if tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM service_template_uploads WHERE principal=?1 AND begin_key=?2)",
        params![principal, r.idempotency_key],
        |row| row.get::<_, bool>(0),
    )? {
        return Err(DataError::IdempotencyConflict);
    }
    expire_pending_on(tx, 16)?;
    let (count,reserved):(i64,i64)=tx.query_row("SELECT COUNT(*),COALESCE(SUM(total_bytes),0) FROM service_template_uploads WHERE state='open'",[],|row|Ok((row.get(0)?,row.get(1)?)))?;
    let count = nonnegative(count)?;
    let reserved = nonnegative(reserved)?;
    if count >= 16 || reserved.saturating_add(r.total_bytes) > TOTAL_RESERVED_BYTES {
        return Err(DataError::Busy);
    }
    let id = match &r.target {
        TemplateWriteTarget::New { requested_id } if requested_id.trim().is_empty() => {
            Uuid::new_v4().to_string()
        }
        TemplateWriteTarget::New { requested_id } => requested_id.clone(),
        TemplateWriteTarget::Existing { template_id, .. } => template_id.clone(),
    };
    let header = header_on(tx, &id)?;
    match &r.target {
        TemplateWriteTarget::New { .. } if header.state != TemplateDocumentState::Absent => {
            return Err(DataError::Conflict {
                current_revision: header.revision,
            })
        }
        TemplateWriteTarget::Existing {
            expected_revision, ..
        } => check_revision(&header.revision, expected_revision)?,
        _ => {}
    }
    let expiry = Utc::now() + Duration::hours(1);
    tx.execute("INSERT INTO service_template_uploads(id,principal,begin_key,intent_hash,template_id,target_revision,target_generation,name,description,total_bytes,sha256,state,expires_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,'open',?12)",params![r.upload_id.to_string(),principal,r.idempotency_key,intent,id,header.revision,header.generation,r.name,r.description,sql_integer(r.total_bytes)?,r.sha256,expiry])?;
    Ok(DesktopTemplatesReply::Upload(
        upload_on(tx, r.upload_id, principal)?.public,
    ))
}
fn append_on(
    tx: &Transaction<'_>,
    principal: &str,
    r: &TemplateAppendWrite,
) -> DataResult<DesktopTemplatesReply> {
    let digest = fingerprint(r)?;
    let saved: Option<(String, DateTime<Utc>)> = tx.query_row(
        "SELECT arguments_hash,expires_at FROM service_idempotency WHERE principal=?1 AND tool='template_append' AND key=?2",
        params![principal,r.idempotency_key], |row| Ok((row.get(0)?,row.get(1)?)),
    ).optional()?;
    if let Some((saved, until)) = saved {
        if until > Utc::now() {
            if saved != digest {
                return Err(DataError::IdempotencyConflict);
            }
            // Upload progress is a live projection, not an immutable Open
            // response cached before a later commit, abort or expiry.
            return Ok(DesktopTemplatesReply::Upload(
                expire_on(tx, upload_on(tx, r.upload_id, principal)?)?.public,
            ));
        }
        tx.execute("DELETE FROM service_idempotency WHERE principal=?1 AND tool='template_append' AND key=?2",params![principal,r.idempotency_key])?;
    }
    if sha(r.text.as_bytes()) != r.sha256 {
        return Err(DataError::Invalid("template chunk digest"));
    }
    let result = append_current_on(tx, principal, r)?;
    // Retain only the closed input fingerprint. Body-bearing chunks and upload
    // metadata may be purged independently without releasing this retry key.
    tx.execute(
        "INSERT INTO service_idempotency VALUES(?1,'template_append',?2,?3,'{}',?4)",
        params![
            principal,
            r.idempotency_key,
            digest,
            Utc::now() + Duration::seconds(consistency::DEDUP_RETENTION_SECONDS)
        ],
    )?;
    Ok(result)
}
fn append_current_on(
    tx: &Transaction<'_>,
    principal: &str,
    r: &TemplateAppendWrite,
) -> DataResult<DesktopTemplatesReply> {
    let upload = expire_on(tx, upload_on(tx, r.upload_id, principal)?)?;
    if upload.public.status != TemplateUploadStatus::Open {
        return Ok(DesktopTemplatesReply::Upload(upload.public));
    }
    let saved:Option<(u32,i64,String,String)>=tx.query_row("SELECT sequence,byte_offset,sha256,append_key FROM service_template_chunks WHERE upload_id=?1 AND (sequence=?2 OR append_key=?3)",params![r.upload_id.to_string(),r.sequence,r.idempotency_key],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?))).optional()?;
    if let Some(saved) = saved {
        let saved = (saved.0, nonnegative(saved.1)?, saved.2, saved.3);
        if saved
            != (
                r.sequence,
                r.byte_offset,
                r.sha256.clone(),
                r.idempotency_key.clone(),
            )
        {
            return Err(DataError::IdempotencyConflict);
        }
        return Ok(DesktopTemplatesReply::Upload(upload.public));
    }
    if r.sequence != upload.public.next_sequence
        || r.byte_offset != upload.public.received_bytes
        || r.byte_offset + r.text.len() as u64 > upload.public.total_bytes
    {
        return Err(DataError::Invalid("template chunk order or length"));
    }
    tx.execute(
        "INSERT INTO service_template_chunks VALUES(?1,?2,?3,?4,?5,?6)",
        params![
            r.upload_id.to_string(),
            r.sequence,
            sql_integer(r.byte_offset)?,
            r.text,
            r.sha256,
            r.idempotency_key
        ],
    )?;
    tx.execute(
        "UPDATE service_template_uploads SET received_bytes=?2,next_sequence=?3 WHERE id=?1",
        params![
            r.upload_id.to_string(),
            sql_integer(r.byte_offset + r.text.len() as u64)?,
            r.sequence + 1
        ],
    )?;
    Ok(DesktopTemplatesReply::Upload(
        upload_on(tx, r.upload_id, principal)?.public,
    ))
}
fn dedup<R: Serialize + DeserializeOwned>(
    tx: &Transaction<'_>,
    principal: &str,
    tool: &str,
    key: &str,
    payload: &impl Serialize,
    action: impl FnOnce() -> DataResult<R>,
) -> DataResult<R> {
    let digest = fingerprint(payload)?;
    let saved:Option<(String,Option<String>,DateTime<Utc>)>=tx.query_row("SELECT arguments_hash,CASE WHEN length(CAST(outcome_json AS BLOB))<=65024 THEN outcome_json END,expires_at FROM service_idempotency WHERE principal=?1 AND tool=?2 AND key=?3",params![principal,tool,key],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?))).optional()?;
    if let Some((hash, body, expires)) = saved {
        if expires > Utc::now() {
            if hash != digest {
                return Err(DataError::IdempotencyConflict);
            }
            return Ok(serde_json::from_str(
                body.as_deref()
                    .ok_or(DataError::Invalid("template receipt budget"))?,
            )?);
        }
        tx.execute(
            "DELETE FROM service_idempotency WHERE principal=?1 AND tool=?2 AND key=?3",
            params![principal, tool, key],
        )?;
    }
    let result = action()?;
    if !template_body_fits(&result, MAX_TEMPLATE_DATA_BYTES) {
        return Err(DataError::Invalid("template receipt budget"));
    }
    tx.execute(
        "INSERT INTO service_idempotency VALUES(?1,?2,?3,?4,?5,?6)",
        params![
            principal,
            tool,
            key,
            digest,
            serde_json::to_string(&result)?,
            Utc::now() + Duration::seconds(consistency::DEDUP_RETENTION_SECONDS)
        ],
    )?;
    Ok(result)
}
fn receipt(
    header: &TemplateHeader,
    effect: TemplateEffect,
    sequence: Option<i64>,
) -> TemplateReceipt {
    TemplateReceipt {
        upload_id: None,
        template_id: Some(header.id.clone()),
        revision: header.revision.clone(),
        generation: Some(header.generation.clone()),
        effect,
        change_sequence: sequence,
        inserted_ids: Vec::new(),
        dedup_until: Utc::now() + Duration::seconds(consistency::DEDUP_RETENTION_SECONDS),
    }
}
fn commit_on(
    tx: &Transaction<'_>,
    principal: &str,
    r: &TemplateUploadRequest,
) -> DataResult<DesktopTemplatesReply> {
    dedup(
        tx,
        principal,
        "template_commit",
        &r.idempotency_key,
        r,
        || {
            let upload = expire_on(tx, upload_on(tx, r.upload_id, principal)?)?;
            if upload.public.status == TemplateUploadStatus::Committed {
                return Ok(DesktopTemplatesReply::Mutation(
                    upload
                        .public
                        .receipt
                        .ok_or(DataError::Invalid("template commit receipt"))?,
                ));
            }
            if upload.public.status != TemplateUploadStatus::Open {
                return Ok(DesktopTemplatesReply::Upload(upload.public));
            }
            if upload.public.received_bytes != upload.public.total_bytes {
                return Err(DataError::Invalid("template upload incomplete"));
            }
            let header = header_on(tx, &upload.public.template_id)?;
            check_revision(&header.revision, &upload.target_revision)?;
            if header.generation != upload.target_generation {
                return Err(DataError::StaleCursor);
            }
            let mut digest = Sha256::new();
            let mut bytes = 0u64;
            let mut last_sequence = 0u32;
            let mut carry = Vec::new();
            let mut has_marker = false;
            {
                let mut statement=tx.prepare("SELECT sequence,byte_offset,CASE WHEN length(CAST(text AS BLOB))<=32768 THEN text END,sha256 FROM service_template_chunks WHERE upload_id=?1 ORDER BY sequence LIMIT 4097")?;
                let rows = statement.query_map([r.upload_id.to_string()], |row| {
                    Ok((
                        row.get::<_, u32>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                })?;
                for (sequence, row) in (1u32..).zip(rows) {
                    let (s, offset, text, chunk_digest) = row?;
                    let offset = nonnegative(offset)?;
                    let text = text.ok_or(DataError::Invalid("stored template chunk budget"))?;
                    if s > MAX_TEMPLATE_CHUNKS
                        || s != sequence
                        || offset != bytes
                        || text.is_empty()
                        || sha(text.as_bytes()) != chunk_digest
                    {
                        return Err(DataError::Invalid("stored template chunk integrity"));
                    }
                    last_sequence = sequence;
                    bytes += text.len() as u64;
                    digest.update(text.as_bytes());
                    carry.extend_from_slice(text.as_bytes());
                    has_marker |= carry
                        .windows(b"{{transcript}}".len())
                        .any(|w| w == b"{{transcript}}");
                    if carry.len() > 13 {
                        carry.drain(..carry.len() - 13);
                    }
                }
            }
            if last_sequence + 1 != upload.public.next_sequence
                || bytes != upload.public.total_bytes
                || format!("{:x}", digest.finalize()) != upload.sha256
            {
                return Err(DataError::Invalid("template upload digest"));
            }
            if !has_marker {
                return Err(DataError::Invalid("template_transcript"));
            }
            let name = upload
                .name
                .ok_or(DataError::Invalid("stored template metadata"))?;
            let description = upload
                .description
                .ok_or(DataError::Invalid("stored template metadata"))?;
            if name.trim().is_empty() {
                return Err(DataError::Invalid("template_name"));
            }
            tx.execute("INSERT INTO templates(id,name,description,body,builtin,created_at) VALUES(?1,?2,?3,COALESCE((SELECT group_concat(text,'') FROM (SELECT text FROM service_template_chunks WHERE upload_id=?4 ORDER BY sequence)),''),?5,?6) ON CONFLICT(id) DO UPDATE SET name=excluded.name,description=excluded.description,body=excluded.body,builtin=excluded.builtin",params![header.id,name,description,r.upload_id.to_string(),header.state==TemplateDocumentState::Present&&header.builtin,Utc::now()])?;
            let generation = if header.state == TemplateDocumentState::Present {
                header.generation
            } else {
                revision()
            };
            let rev = revision();
            tx.execute("INSERT INTO service_template_state VALUES(?1,?2,?3,'present',?4) ON CONFLICT(id) DO UPDATE SET revision=excluded.revision,generation=excluded.generation,state='present',builtin=excluded.builtin",params![header.id,rev,generation,header.builtin&&header.state==TemplateDocumentState::Present])?;
            bump_catalog(tx)?;
            let current = header_on(tx, &header.id)?;
            let seq =
                consistency::change(tx, "template", &current.id, &current.revision, principal)?;
            let mut result = receipt(&current, TemplateEffect::Saved, Some(seq));
            result.upload_id = Some(r.upload_id);
            tx.execute(
                "DELETE FROM service_template_chunks WHERE upload_id=?1",
                [r.upload_id.to_string()],
            )?;
            tx.execute("UPDATE service_template_uploads SET state='committed',name=NULL,description=NULL,receipt_json=?2 WHERE id=?1",params![r.upload_id.to_string(),serde_json::to_string(&result)?])?;
            Ok(DesktopTemplatesReply::Mutation(result))
        },
    )
}
fn delete_on(
    tx: &Transaction<'_>,
    principal: &str,
    r: &TemplateDeleteRequest,
) -> DataResult<DesktopTemplatesReply> {
    dedup(
        tx,
        principal,
        "template_delete",
        &r.idempotency_key,
        r,
        || {
            let mut header = header_on(tx, &r.template_id)?;
            check_revision(&header.revision, &r.expected_revision)?;
            if header.state != TemplateDocumentState::Present {
                return Ok(DesktopTemplatesReply::Mutation(receipt(
                    &header,
                    TemplateEffect::AlreadyAbsent,
                    None,
                )));
            }
            if header.builtin {
                return Ok(DesktopTemplatesReply::Mutation(receipt(
                    &header,
                    TemplateEffect::BuiltinProtectedNoOp,
                    None,
                )));
            }
            tx.execute("DELETE FROM templates WHERE id=?1", [&header.id])?;
            header.revision = revision();
            tx.execute(
                "UPDATE service_template_state SET state='deleted',revision=?2 WHERE id=?1",
                params![header.id, header.revision],
            )?;
            bump_catalog(tx)?;
            let seq = consistency::change(tx, "template", &header.id, &header.revision, principal)?;
            Ok(DesktopTemplatesReply::Mutation(receipt(
                &header,
                TemplateEffect::Deleted,
                Some(seq),
            )))
        },
    )
}
fn seed_on(
    tx: &Transaction<'_>,
    principal: &str,
    r: &TemplateSeedRequest,
) -> DataResult<DesktopTemplatesReply> {
    dedup(
        tx,
        principal,
        "template_seed_missing",
        &r.idempotency_key,
        r,
        || {
            let catalog = catalog_on(tx)?;
            check_revision(&catalog, &r.expected_catalog_revision)?;
            let mut inserted = Vec::new();
            for builtin in bazza_templates::builtin_templates() {
                let old = header_on(tx, builtin.id)?;
                if old.state == TemplateDocumentState::Present {
                    continue;
                }
                tx.execute("INSERT INTO templates(id,name,description,body,builtin,created_at) VALUES(?1,?2,?3,?4,1,?5)",params![builtin.id,builtin.name,builtin.description,builtin.body,Utc::now()])?;
                tx.execute("INSERT INTO service_template_state VALUES(?1,?2,?3,'present',1) ON CONFLICT(id) DO UPDATE SET revision=excluded.revision,generation=excluded.generation,state='present',builtin=1",params![builtin.id,revision(),revision()])?;
                inserted.push(builtin.id.to_owned());
            }
            let (rev, sequence) = if inserted.is_empty() {
                (catalog, None)
            } else {
                let rev = bump_catalog(tx)?;
                let sequence =
                    consistency::change(tx, "templates_seed", "templates", &rev, principal)?;
                (rev, Some(sequence))
            };
            Ok(DesktopTemplatesReply::Mutation(TemplateReceipt {
                upload_id: None,
                template_id: None,
                revision: rev,
                generation: None,
                effect: TemplateEffect::SeededMissing,
                change_sequence: sequence,
                inserted_ids: inserted,
                dedup_until: Utc::now() + Duration::seconds(consistency::DEDUP_RETENTION_SECONDS),
            }))
        },
    )
}

impl Store {
    /// Trusted private application principal; no caller-selected authority/key/SQL.
    pub fn service_desktop_templates(
        &self,
        principal: &str,
        request: &DesktopTemplatesRequest,
    ) -> DataResult<DesktopTemplatesReply> {
        bounded_text(principal, 1, 128)?;
        if matches!(request,DesktopTemplatesRequest::BeginWrite(r) if r.name.trim().is_empty()) {
            return Err(DataError::Invalid("template_name"));
        }
        if !request.valid() {
            return Err(DataError::Invalid("template request"));
        }
        let read = matches!(
            request,
            DesktopTemplatesRequest::List(_)
                | DesktopTemplatesRequest::Describe { .. }
                | DesktopTemplatesRequest::ReadField(_)
        );
        let mut conn = self.conn.lock();
        let tx = conn.transaction_with_behavior(if read {
            TransactionBehavior::Deferred
        } else {
            TransactionBehavior::Immediate
        })?;
        let result = match request {
            DesktopTemplatesRequest::List(r) => list_on(&tx, r)?,
            DesktopTemplatesRequest::Describe { template_id } => {
                DesktopTemplatesReply::Describe(header_on(&tx, template_id)?)
            }
            DesktopTemplatesRequest::ReadField(r) => field_on(&tx, r)?,
            DesktopTemplatesRequest::BeginWrite(r) => begin_on(&tx, principal, r)?,
            DesktopTemplatesRequest::AppendWrite(r) => append_on(&tx, principal, r)?,
            DesktopTemplatesRequest::CommitWrite(r) => commit_on(&tx, principal, r)?,
            DesktopTemplatesRequest::AbortWrite(r) => dedup(
                &tx,
                principal,
                "template_abort",
                &r.idempotency_key,
                r,
                || {
                    let upload = expire_on(&tx, upload_on(&tx, r.upload_id, principal)?)?;
                    if upload.public.status == TemplateUploadStatus::Open {
                        retire_on(&tx, r.upload_id, "aborted")?;
                    }
                    Ok(DesktopTemplatesReply::Upload(
                        upload_on(&tx, r.upload_id, principal)?.public,
                    ))
                },
            )?,
            DesktopTemplatesRequest::Delete(r) => delete_on(&tx, principal, r)?,
            DesktopTemplatesRequest::SeedMissingBuiltins(r) => seed_on(&tx, principal, r)?,
        };
        if !result.valid_for(request) {
            return Err(DataError::Invalid("template reply contract"));
        }
        if !read {
            tx.commit()?;
        }
        Ok(result)
    }
    /// Explicit service housekeeping, bounded by active-upload admission (16).
    pub fn service_expire_template_uploads(&self, limit: u16) -> DataResult<u32> {
        if !(1..=32).contains(&limit) {
            return Err(DataError::Invalid("template expiry limit"));
        }
        let mut conn = self.conn.lock();
        // Most maintenance ticks have no work. Do not start a write transaction
        // or scan retained upload identities when nothing is due.
        let due: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM service_template_uploads WHERE state='open' AND expires_at<=?1)",
            [Utc::now()],
            |row| row.get(0),
        )?;
        if !due {
            return Ok(0);
        }
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let count = expire_pending_on(&tx, limit)?;
        tx.commit()?;
        Ok(count)
    }
}

#[cfg(test)]
#[path = "templates_tests.rs"]
mod tests;

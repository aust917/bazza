//! Persistent whole-library read grants. Only the owning service calls these
//! methods. The external RPC handler must hold its authorization gate through
//! response publication and take the exclusive gate for admin revocation.
use super::{bounded_text, DataError, DataResult};
use crate::Store;
use chrono::{DateTime, Utc};
use rusqlite::{params, OptionalExtension, Row, TransactionBehavior};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{fmt, io};
use uuid::Uuid;

const MAX_LABEL_BYTES: usize = 128;
const MAX_GRANT_PAGE: u16 = 100;

/// Client-held bearer. Database values and metadata responses never contain it.
/// Only the pairing flow should use `expose` to install it in its credential store.
pub struct ExternalReadSecret(String);

impl fmt::Debug for ExternalReadSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ExternalReadSecret([redacted])")
    }
}

impl ExternalReadSecret {
    pub fn parse(value: String) -> DataResult<Self> {
        if value.len() != 64
            || !value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(DataError::Invalid("external read credential"));
        }
        Ok(Self(value))
    }

    pub fn expose(&self) -> &str {
        &self.0
    }

    fn generate() -> DataResult<Self> {
        let mut bytes = [0_u8; 32];
        getrandom::fill(&mut bytes).map_err(|_| io::Error::other("external read entropy"))?;
        Ok(Self(
            bytes.iter().map(|byte| format!("{byte:02x}")).collect(),
        ))
    }

    fn digest(&self, library_id: Uuid, connection_id: Uuid) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(b"bazza-external-read-grant-v1\0");
        hasher.update(library_id.as_bytes());
        hasher.update(connection_id.as_bytes());
        hasher.update(self.0.as_bytes());
        hasher.finalize().into()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalGrantPermission {
    Read,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ExternalReadGrant {
    pub library_id: Uuid,
    pub connection_id: Uuid,
    pub client_label: String,
    pub permission: ExternalGrantPermission,
    pub created_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
}

/// Pairing returns this only once. Subsequent admin reads return metadata.
#[derive(Debug)]
pub struct IssuedExternalReadGrant {
    pub grant: ExternalReadGrant,
    pub secret: ExternalReadSecret,
}

fn library_id_valid(library_id: Uuid) -> DataResult<()> {
    if library_id.is_nil() {
        return Err(DataError::Invalid("external read library identity"));
    }
    Ok(())
}

fn grant_row(library_id: Uuid, row: &Row<'_>) -> rusqlite::Result<ExternalReadGrant> {
    let connection: String = row.get(0)?;
    let connection_id = Uuid::parse_str(&connection)
        .ok()
        .filter(|id| !id.is_nil() && id.to_string() == connection)
        .ok_or(rusqlite::Error::InvalidQuery)?;
    Ok(ExternalReadGrant {
        library_id,
        connection_id,
        client_label: row.get(1)?,
        permission: ExternalGrantPermission::Read,
        created_at: row.get(2)?,
        revoked_at: row.get(3)?,
    })
}

impl Store {
    /// Called only after explicit local admin pairing for this library. The
    /// caller takes `library_id` from the owning repository, not the wire.
    pub fn service_issue_external_read_grant(
        &self,
        library_id: Uuid,
        client_label: &str,
    ) -> DataResult<IssuedExternalReadGrant> {
        library_id_valid(library_id)?;
        bounded_text(client_label, 1, MAX_LABEL_BYTES)?;
        if client_label.trim().is_empty() || client_label.chars().any(char::is_control) {
            return Err(DataError::Invalid("external read client label"));
        }
        let connection_id = Uuid::new_v4();
        let secret = ExternalReadSecret::generate()?;
        let created_at = Utc::now();
        self.conn.lock().execute(
            "INSERT INTO service_external_read_grants(
                 connection_id,library_id,client_label,permission,credential_digest,created_at
             ) VALUES (?1,?2,?3,'read',?4,?5)",
            params![
                connection_id.to_string(),
                library_id.to_string(),
                client_label,
                &secret.digest(library_id, connection_id)[..],
                created_at
            ],
        )?;
        Ok(IssuedExternalReadGrant {
            grant: ExternalReadGrant {
                library_id,
                connection_id,
                client_label: client_label.into(),
                permission: ExternalGrantPermission::Read,
                created_at,
                revoked_at: None,
            },
            secret,
        })
    }

    pub fn service_list_external_read_grants(
        &self,
        library_id: Uuid,
        after: Option<Uuid>,
        limit: u16,
    ) -> DataResult<Vec<ExternalReadGrant>> {
        library_id_valid(library_id)?;
        if !(1..=MAX_GRANT_PAGE).contains(&limit) || after.is_some_and(|id| id.is_nil()) {
            return Err(DataError::Invalid("external read grant page"));
        }
        let conn = self.conn.lock();
        let mut statement = conn.prepare_cached(
            "SELECT connection_id,client_label,created_at,revoked_at
             FROM service_external_read_grants
             WHERE library_id=?1 AND connection_id>?2 AND permission='read'
             ORDER BY connection_id LIMIT ?3",
        )?;
        let rows = statement.query_map(
            params![
                library_id.to_string(),
                after.map(|id| id.to_string()).unwrap_or_default(),
                limit
            ],
            |row| grant_row(library_id, row),
        )?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Idempotent revocation retains the original timestamp and audit metadata.
    pub fn service_revoke_external_read_grant(
        &self,
        library_id: Uuid,
        connection_id: Uuid,
    ) -> DataResult<ExternalReadGrant> {
        library_id_valid(library_id)?;
        if connection_id.is_nil() {
            return Err(DataError::Invalid("external read connection identity"));
        }
        let mut conn = self.conn.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "UPDATE service_external_read_grants SET revoked_at=?3
             WHERE library_id=?1 AND connection_id=?2 AND permission='read' AND revoked_at IS NULL",
            params![
                library_id.to_string(),
                connection_id.to_string(),
                Utc::now()
            ],
        )?;
        let grant = tx
            .query_row(
                "SELECT connection_id,client_label,created_at,revoked_at
                 FROM service_external_read_grants
                 WHERE library_id=?1 AND connection_id=?2 AND permission='read'",
                params![library_id.to_string(), connection_id.to_string()],
                |row| grant_row(library_id, row),
            )
            .optional()?
            .ok_or(DataError::NotFound)?;
        tx.commit()?;
        Ok(grant)
    }

    /// Call for every external read, even on an existing IPC connection. The
    /// transport also verifies its peer and the selected library and epoch.
    pub fn service_authorize_external_read(
        &self,
        library_id: Uuid,
        connection_id: Uuid,
        secret: &ExternalReadSecret,
    ) -> DataResult<()> {
        if library_id.is_nil() || connection_id.is_nil() {
            return Err(DataError::Forbidden);
        }
        let actual = secret.digest(library_id, connection_id);
        let expected: [u8; 32] = self
            .conn
            .lock()
            .query_row(
                "SELECT credential_digest FROM service_external_read_grants
                 WHERE library_id=?1 AND connection_id=?2 AND permission='read'
                   AND revoked_at IS NULL",
                params![library_id.to_string(), connection_id.to_string()],
                |row| row.get(0),
            )
            .optional()?
            .ok_or(DataError::Forbidden)?;
        let difference = expected
            .iter()
            .zip(actual)
            .fold(0, |bits, (&left, right)| bits | (left ^ right));
        if difference != 0 {
            return Err(DataError::Forbidden);
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "external_read_grants_tests.rs"]
mod tests;

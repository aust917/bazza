//! OS-managed external MCP read credentials. The key includes the persistent
//! library identity and the connection ID issued by the service.

use bazza_service_api::worker::WorkerSecret;
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum CredentialError {
    #[error("invalid MCP credential identity")]
    InvalidIdentity,
    #[error("the OS credential store is unavailable")]
    Unavailable,
    #[error("the stored MCP credential is invalid")]
    Malformed,
    #[error("the OS credential store is unsupported on this platform")]
    Unsupported,
}

#[cfg(any(windows, test))]
fn target_name(library_id: Uuid, connection_id: Uuid) -> Result<String, CredentialError> {
    if library_id.is_nil() || connection_id.is_nil() {
        return Err(CredentialError::InvalidIdentity);
    }
    Ok(format!(
        "Bazza:MCP:read:v1:{}:{}",
        library_id.simple(),
        connection_id.simple()
    ))
}

#[cfg(windows)]
pub use windows::{load, remove, store};

#[cfg(windows)]
mod windows {
    use super::*;
    use std::{ptr, slice, str};
    use windows_sys::Win32::{
        Foundation::{GetLastError, ERROR_NOT_FOUND},
        Security::Credentials::{
            CredDeleteW, CredFree, CredReadW, CredWriteW, CREDENTIALW, CRED_PERSIST_LOCAL_MACHINE,
            CRED_TYPE_GENERIC,
        },
    };

    const SECRET_BYTES: usize = 64;

    fn target(library_id: Uuid, connection_id: Uuid) -> Result<Vec<u16>, CredentialError> {
        Ok(target_name(library_id, connection_id)?
            .encode_utf16()
            .chain([0])
            .collect())
    }

    /// Save the secret for this Windows user on this machine.
    pub fn store(
        library_id: Uuid,
        connection_id: Uuid,
        secret: &WorkerSecret,
    ) -> Result<(), CredentialError> {
        let mut target = target(library_id, connection_id)?;
        let blob = secret.expose().as_bytes();
        if blob.len() != SECRET_BYTES {
            return Err(CredentialError::Malformed);
        }
        let credential = CREDENTIALW {
            Type: CRED_TYPE_GENERIC,
            TargetName: target.as_mut_ptr(),
            CredentialBlobSize: SECRET_BYTES as u32,
            CredentialBlob: blob.as_ptr().cast_mut(),
            Persist: CRED_PERSIST_LOCAL_MACHINE,
            ..Default::default()
        };
        // SAFETY: the target is NUL-terminated, the blob is bounded, and the
        // pointers remain live through the synchronous input-only Win32 call.
        if unsafe { CredWriteW(&credential, 0) } == 0 {
            return Err(CredentialError::Unavailable);
        }
        Ok(())
    }

    struct Allocation(*mut CREDENTIALW);

    impl Drop for Allocation {
        fn drop(&mut self) {
            // SAFETY: this owns the one block returned by CredReadW. The
            // validated secret blob is part of the same writable allocation.
            unsafe {
                let entry = &*self.0;
                if entry.CredentialBlobSize == SECRET_BYTES as u32
                    && !entry.CredentialBlob.is_null()
                {
                    for offset in 0..SECRET_BYTES {
                        entry.CredentialBlob.add(offset).write_volatile(0);
                    }
                }
                CredFree(self.0.cast());
            }
        }
    }

    /// Look up only the exact issued library and connection pair.
    pub fn load(
        library_id: Uuid,
        connection_id: Uuid,
    ) -> Result<Option<WorkerSecret>, CredentialError> {
        let target = target(library_id, connection_id)?;
        let mut raw = ptr::null_mut();
        // SAFETY: target is NUL-terminated and raw is an initialized output
        // pointer. A successful result is owned and freed by Allocation.
        if unsafe { CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &mut raw) } == 0 {
            // SAFETY: read the last error immediately after the failed call.
            return if unsafe { GetLastError() } == ERROR_NOT_FOUND {
                Ok(None)
            } else {
                Err(CredentialError::Unavailable)
            };
        }
        if raw.is_null() {
            return Err(CredentialError::Malformed);
        }
        let allocation = Allocation(raw);
        // SAFETY: CredReadW guarantees that a successful non-null pointer
        // refers to a CREDENTIALW in the live allocation.
        let entry = unsafe { &*allocation.0 };
        if entry.Type != CRED_TYPE_GENERIC
            || entry.CredentialBlobSize != SECRET_BYTES as u32
            || entry.CredentialBlob.is_null()
        {
            return Err(CredentialError::Malformed);
        }
        // SAFETY: CredReadW supplies the blob and the length was checked above.
        let blob = unsafe { slice::from_raw_parts(entry.CredentialBlob, SECRET_BYTES) };
        let value = str::from_utf8(blob).map_err(|_| CredentialError::Malformed)?;
        WorkerSecret::from_string(value.to_owned())
            .map(Some)
            .map_err(|_| CredentialError::Malformed)
    }

    /// Delete the local copy after the service has revoked the grant.
    pub fn remove(library_id: Uuid, connection_id: Uuid) -> Result<(), CredentialError> {
        let target = target(library_id, connection_id)?;
        // SAFETY: target is NUL-terminated and remains live for this call.
        if unsafe { CredDeleteW(target.as_ptr(), CRED_TYPE_GENERIC, 0) } != 0 {
            return Ok(());
        }
        // SAFETY: read the last error immediately after the failed call.
        if unsafe { GetLastError() } == ERROR_NOT_FOUND {
            Ok(())
        } else {
            Err(CredentialError::Unavailable)
        }
    }
}

#[cfg(not(windows))]
pub fn store(
    _library_id: Uuid,
    _connection_id: Uuid,
    _secret: &WorkerSecret,
) -> Result<(), CredentialError> {
    Err(CredentialError::Unsupported)
}

#[cfg(not(windows))]
pub fn load(
    _library_id: Uuid,
    _connection_id: Uuid,
) -> Result<Option<WorkerSecret>, CredentialError> {
    Err(CredentialError::Unsupported)
}

#[cfg(not(windows))]
pub fn remove(_library_id: Uuid, _connection_id: Uuid) -> Result<(), CredentialError> {
    Err(CredentialError::Unsupported)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_is_namespaced_by_both_nonnil_identifiers() {
        let library = Uuid::from_u128(1);
        let connection = Uuid::from_u128(2);
        let target = target_name(library, connection).unwrap();
        assert_eq!(
            target,
            "Bazza:MCP:read:v1:00000000000000000000000000000001:00000000000000000000000000000002"
        );
        assert_ne!(target_name(connection, library).unwrap(), target);
        assert_ne!(target_name(library, Uuid::from_u128(3)).unwrap(), target);
        assert!(target_name(Uuid::nil(), connection).is_err());
        assert!(target_name(library, Uuid::nil()).is_err());
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "requires the current Windows user's credential vault; uses unique IDs and removes its own item"]
    fn isolated_windows_credential_roundtrip() {
        struct Cleanup(Uuid, Uuid);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = remove(self.0, self.1);
            }
        }
        let cleanup = Cleanup(Uuid::new_v4(), Uuid::new_v4());
        let secret = WorkerSecret::random();
        assert!(load(cleanup.0, cleanup.1).unwrap().is_none());
        store(cleanup.0, cleanup.1, &secret).unwrap();
        assert_eq!(load(cleanup.0, cleanup.1).unwrap(), Some(secret));
        assert!(load(Uuid::new_v4(), cleanup.1).unwrap().is_none());
        remove(cleanup.0, cleanup.1).unwrap();
        assert!(load(cleanup.0, cleanup.1).unwrap().is_none());
        remove(cleanup.0, cleanup.1).unwrap();
    }
}

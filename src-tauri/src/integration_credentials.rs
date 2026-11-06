//! OS-managed credentials for optional recording integrations.

#[cfg(windows)]
mod platform {
    use std::{ptr, slice};
    use windows::{
        core::{HRESULT, HSTRING, PCWSTR, PWSTR},
        Win32::{
            Foundation::ERROR_NOT_FOUND,
            Security::Credentials::{
                CredDeleteW, CredFree, CredReadW, CredWriteW, CREDENTIALW,
                CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC,
            },
        },
    };

    fn not_found(error: &windows::core::Error) -> bool {
        error.code() == HRESULT::from_win32(ERROR_NOT_FOUND.0)
    }

    pub(super) fn store(_: &str, target: &str, token: &str) -> Result<(), ()> {
        let target = HSTRING::from(target);
        let blob = token.as_bytes();
        let credential = CREDENTIALW {
            Type: CRED_TYPE_GENERIC,
            TargetName: PWSTR(target.as_ptr().cast_mut()),
            CredentialBlobSize: blob.len() as u32,
            CredentialBlob: blob.as_ptr().cast_mut(),
            Persist: CRED_PERSIST_LOCAL_MACHINE,
            ..Default::default()
        };
        // SAFETY: target is NUL-terminated and the input-only blob stays live.
        unsafe { CredWriteW(&credential, 0) }.map_err(|_| ())
    }

    struct Allocation(*mut CREDENTIALW);

    impl Drop for Allocation {
        fn drop(&mut self) {
            // SAFETY: this owns the buffer returned by CredReadW.
            unsafe { CredFree(self.0.cast()) };
        }
    }

    pub(super) fn load(_: &str, target: &str) -> Result<Option<String>, ()> {
        let target = HSTRING::from(target);
        let mut raw = ptr::null_mut();
        // SAFETY: target is NUL-terminated and raw is an initialized out pointer.
        if let Err(error) =
            unsafe { CredReadW(PCWSTR(target.as_ptr()), CRED_TYPE_GENERIC, None, &mut raw) }
        {
            return if not_found(&error) { Ok(None) } else { Err(()) };
        }
        if raw.is_null() {
            return Err(());
        }
        let allocation = Allocation(raw);
        // SAFETY: CredReadW returned a live CREDENTIALW allocation.
        let entry = unsafe { &*allocation.0 };
        let size = entry.CredentialBlobSize as usize;
        if entry.Type != CRED_TYPE_GENERIC
            || !(1..=super::MAX_TOKEN_BYTES).contains(&size)
            || entry.CredentialBlob.is_null()
        {
            return Err(());
        }
        // SAFETY: the non-null blob and its length are owned by the credential.
        let blob = unsafe { slice::from_raw_parts(entry.CredentialBlob, size) };
        let value = std::str::from_utf8(blob).map_err(|_| ())?.to_owned();
        Ok(Some(value))
    }

    pub(super) fn remove(_: &str, target: &str) -> Result<(), ()> {
        let target = HSTRING::from(target);
        // SAFETY: target is NUL-terminated and remains live during the call.
        match unsafe { CredDeleteW(PCWSTR(target.as_ptr()), CRED_TYPE_GENERIC, None) } {
            Ok(()) => Ok(()),
            Err(error) if not_found(&error) => Ok(()),
            Err(_) => Err(()),
        }
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use security_framework::passwords::{
        delete_generic_password, generic_password, set_generic_password,
    };

    const ERR_SEC_ITEM_NOT_FOUND: i32 = -25300;

    pub(super) fn store(service: &str, target: &str, token: &str) -> Result<(), ()> {
        set_generic_password(service, target, token.as_bytes()).map_err(|_| ())
    }

    pub(super) fn load(service: &str, target: &str) -> Result<Option<String>, ()> {
        match generic_password(service, target) {
            Ok(blob) if (1..=super::MAX_TOKEN_BYTES).contains(&blob.len()) => {
                String::from_utf8(blob).map(Some).map_err(|_| ())
            }
            Err(error) if error.code() == ERR_SEC_ITEM_NOT_FOUND => Ok(None),
            _ => Err(()),
        }
    }

    pub(super) fn remove(service: &str, target: &str) -> Result<(), ()> {
        match delete_generic_password(service, target) {
            Ok(()) => Ok(()),
            Err(error) if error.code() == ERR_SEC_ITEM_NOT_FOUND => Ok(()),
            Err(_) => Err(()),
        }
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
mod platform {
    pub(super) fn store(_: &str, _: &str, _: &str) -> Result<(), ()> {
        Err(())
    }

    pub(super) fn load(_: &str, _: &str) -> Result<Option<String>, ()> {
        Err(())
    }

    pub(super) fn remove(_: &str, _: &str) -> Result<(), ()> {
        Err(())
    }
}

pub const MAX_TOKEN_BYTES: usize = 512;

pub fn store(service: &str, target: &str, token: &str) -> Result<(), ()> {
    if !(1..=MAX_TOKEN_BYTES).contains(&token.len()) {
        return Err(());
    }
    platform::store(service, target, token)
}

pub fn load(service: &str, target: &str) -> Result<Option<String>, ()> {
    platform::load(service, target)
}

pub fn remove(service: &str, target: &str) -> Result<(), ()> {
    platform::remove(service, target)
}

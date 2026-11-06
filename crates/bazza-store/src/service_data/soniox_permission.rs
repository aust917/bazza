//! SQLite authority for short Soniox permits. Capture identity, current
//! consent/credential generation and issuance are checked in the same transaction.

use super::{configuration, workers, *};
use crate::Store;
use bazza_domain::capture_stt::{
    SonioxCredential, SonioxIncompleteReason, SonioxPermissionPermit, SonioxPermissionReply,
    SONIOX_PERMISSION_MAX_MS,
};
use chrono::{DateTime, Duration, Utc};
use rusqlite::{params, TransactionBehavior};
use uuid::Uuid;

pub enum SonioxPermissionAction {
    Issue,
    Renew(Uuid),
    AckDrained(Uuid),
}

struct Grant {
    library_epoch: Uuid,
    permission_epoch: Uuid,
    expires_at: DateTime<Utc>,
    drained: bool,
}

fn parse_epoch(value: &str) -> DataResult<Uuid> {
    Uuid::parse_str(value)
        .ok()
        .filter(|epoch| !epoch.is_nil())
        .ok_or(DataError::Invalid("stored Soniox permission epoch"))
}

fn authority_on(conn: &Connection) -> DataResult<Uuid> {
    let epoch: String = conn.query_row(
        "SELECT permission_epoch FROM service_soniox_authority WHERE singleton=1",
        [],
        |row| row.get(0),
    )?;
    parse_epoch(&epoch)
}

fn grant_on(conn: &Connection, capture_id: Uuid) -> DataResult<Option<Grant>> {
    let row: Option<(String, String, DateTime<Utc>, bool)> = conn
        .query_row(
            "SELECT library_epoch,permission_epoch,expires_at,drained_at IS NOT NULL
             FROM service_soniox_permissions WHERE capture_id=?1",
            [capture_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    row.map(|(library, permission, expires_at, drained)| {
        Ok(Grant {
            library_epoch: parse_epoch(&library)?,
            permission_epoch: parse_epoch(&permission)?,
            expires_at,
            drained,
        })
    })
    .transpose()
}

fn capture_on(
    conn: &Connection,
    peer: u32,
    epoch: Uuid,
    proof: &WorkerProof,
    lease: &CaptureLease,
    fence: &WorkerSecret,
    acknowledgement: bool,
) -> DataResult<(RegisteredWorker, CaptureSelection)> {
    let worker = workers::authenticate(conn, peer, epoch, proof, acknowledgement)?;
    if !worker.capabilities.contains(&WorkerCapability::Capture) {
        return Err(DataError::Forbidden);
    }
    if acknowledgement {
        // A late close-only ACK may arrive after successful media finalization.
        workers::capture_identity_authority(conn, &worker, lease, fence)?;
    } else {
        workers::capture_authority(conn, &worker, lease, fence)?;
    }
    let (state, raw): (String, String) = conn.query_row(
        "SELECT state,CASE WHEN length(CAST(provider_json AS BLOB))<=1024 THEN provider_json END
         FROM service_capture_leases WHERE id=?1",
        [lease.capture_id.to_string()],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if !(state == "capturing" || acknowledgement && state == "finalized") {
        return Err(DataError::Forbidden);
    }
    let selection: CaptureSelection = serde_json::from_str(&raw)?;
    workers::selection(&selection)?;
    if selection.provider != CaptureProvider::Soniox || selection.model.is_none() {
        return Err(DataError::Forbidden);
    }
    Ok((worker, selection))
}

fn credential_on(conn: &Connection) -> DataResult<SonioxCredential> {
    // Reject oversized legacy rows before allocating, including escaped JSON.
    let value: Option<String> = conn
        .query_row(
            "SELECT CASE WHEN length(CAST(value AS BLOB))<=24578 THEN value END
             FROM settings WHERE key='private:soniox_api_key'",
            [],
            |row| row.get(0),
        )
        .optional()?
        .flatten();
    value
        .as_deref()
        .and_then(|value| serde_json::from_str(value).ok())
        .ok_or(DataError::Forbidden)
}

impl Store {
    pub fn service_soniox_key_present(&self) -> DataResult<bool> {
        match credential_on(&self.conn.lock()) {
            Ok(_) => Ok(true),
            Err(DataError::Forbidden) => Ok(false),
            Err(error) => Err(error),
        }
    }
}

fn valid_for_ms(deadline: DateTime<Utc>, now: DateTime<Utc>) -> Option<u32> {
    let millis = u32::try_from((deadline - now).num_milliseconds()).ok()?;
    (1..=SONIOX_PERMISSION_MAX_MS)
        .contains(&millis)
        .then_some(millis)
}

fn permit(
    lease: &CaptureLease,
    selection: CaptureSelection,
    permission_epoch: Uuid,
    valid_for_ms: u32,
) -> DataResult<SonioxPermissionPermit> {
    Ok(SonioxPermissionPermit {
        capture_id: lease.capture_id,
        meeting_id: lease.meeting_id,
        worker_session_id: lease.worker_session_id,
        generation: lease.generation,
        library_epoch: lease.library_epoch,
        permission_epoch,
        model: selection.model.ok_or(DataError::Forbidden)?,
        language: selection.language,
        valid_for_ms,
    })
}

impl Store {
    /// Core checks this before touching paths or checking the artifact owner.
    /// Issuance and renew repeat the check in the write transaction.
    pub fn service_check_soniox_capture(
        &self,
        peer: u32,
        epoch: Uuid,
        proof: &WorkerProof,
        lease: &CaptureLease,
        fence: &WorkerSecret,
    ) -> DataResult<()> {
        capture_on(&self.conn.lock(), peer, epoch, proof, lease, fence, false)?;
        Ok(())
    }

    pub fn service_soniox_permission(
        &self,
        peer: u32,
        epoch: Uuid,
        proof: &WorkerProof,
        lease: &CaptureLease,
        fence: &WorkerSecret,
        action: SonioxPermissionAction,
    ) -> DataResult<SonioxPermissionReply> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (worker, selection) = capture_on(
            &tx,
            peer,
            epoch,
            proof,
            lease,
            fence,
            matches!(action, SonioxPermissionAction::AckDrained(_)),
        )?;
        let now = Utc::now();
        let deadline = worker
            .expires_at
            .min(now + Duration::milliseconds(i64::from(SONIOX_PERMISSION_MAX_MS)));
        let existing = grant_on(&tx, lease.capture_id)?;
        let reply = match action {
            SonioxPermissionAction::Issue => {
                let permission_epoch = authority_on(&tx)?;
                if configuration::soniox_selection_on(&tx)?.as_ref() != Some(&selection) {
                    return Err(DataError::Forbidden);
                }
                let credential = credential_on(&tx)?;
                let expires = if let Some(existing) = existing {
                    // An exact lost-ACK retry can retrieve the same grant while
                    // live. It cannot extend, revive or adopt an old epoch.
                    if existing.library_epoch != epoch
                        || existing.permission_epoch != permission_epoch
                        || existing.drained
                    {
                        return Err(DataError::Forbidden);
                    }
                    existing.expires_at.min(deadline)
                } else {
                    tx.execute(
                        "INSERT INTO service_soniox_permissions(capture_id,library_epoch,permission_epoch,expires_at)
                         VALUES(?1,?2,?3,?4)",
                        params![lease.capture_id.to_string(), epoch.to_string(), permission_epoch.to_string(), deadline],
                    )?;
                    deadline
                };
                SonioxPermissionReply::Issued {
                    permit: permit(
                        lease,
                        selection,
                        permission_epoch,
                        valid_for_ms(expires, now).ok_or(DataError::Forbidden)?,
                    )?,
                    credential,
                }
            }
            SonioxPermissionAction::Renew(permission_epoch) => {
                let grant = existing
                    .filter(|grant| grant.permission_epoch == permission_epoch)
                    .ok_or(DataError::Forbidden)?;
                if grant.library_epoch != epoch || grant.drained || grant.expires_at <= now {
                    SonioxPermissionReply::Stop {
                        permission_epoch,
                        reason: SonioxIncompleteReason::ControlUnavailable,
                    }
                } else if authority_on(&tx)? != permission_epoch
                    || configuration::soniox_selection_on(&tx)?.as_ref() != Some(&selection)
                {
                    SonioxPermissionReply::Stop {
                        permission_epoch,
                        reason: SonioxIncompleteReason::PermissionRevoked,
                    }
                } else {
                    let valid_for_ms = valid_for_ms(deadline, now).ok_or(DataError::Forbidden)?;
                    tx.execute(
                        "UPDATE service_soniox_permissions SET expires_at=?2 WHERE capture_id=?1",
                        params![lease.capture_id.to_string(), deadline],
                    )?;
                    SonioxPermissionReply::Renewed {
                        permit: permit(lease, selection, permission_epoch, valid_for_ms)?,
                    }
                }
            }
            SonioxPermissionAction::AckDrained(permission_epoch) => {
                existing
                    .filter(|grant| {
                        grant.permission_epoch == permission_epoch && grant.library_epoch == epoch
                    })
                    .ok_or(DataError::Forbidden)?;
                tx.execute(
                    "UPDATE service_soniox_permissions SET drained_at=COALESCE(drained_at,?2) WHERE capture_id=?1",
                    params![lease.capture_id.to_string(), now],
                )?;
                SonioxPermissionReply::Drained { permission_epoch }
            }
        };
        tx.commit()?;
        Ok(reply)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bazza_domain::{NativeSettings, NativeSttProvider};

    const KEY: &str = "private:soniox_api_key";
    const SYNTHETIC: &str = "synthetic-soniox-key-only";

    struct Fixture {
        store: Store,
        settings: NativeSettings,
        proof: WorkerProof,
        lease: CaptureLease,
        fence: WorkerSecret,
    }

    fn fixture() -> Fixture {
        let store = Store::open_in_memory().unwrap();
        let settings = NativeSettings {
            stt_provider: NativeSttProvider::Soniox,
            soniox_consent: true,
            soniox_model: "fixture-soniox".into(),
            language: Some("en".into()),
            ..Default::default()
        };
        store.set_setting("settings", &settings).unwrap();
        store.set_setting(KEY, &SYNTHETIC).unwrap();
        let epoch = Uuid::new_v4();
        let registration = WorkerRegistration {
            session_id: Uuid::new_v4(),
            secret: WorkerSecret::random(),
            capabilities: vec![WorkerCapability::Capture],
        };
        store
            .service_register_worker(42, epoch, &registration, 60)
            .unwrap();
        let proof = WorkerProof {
            session_id: registration.session_id,
            secret: registration.secret,
            library_epoch: epoch,
        };
        let start = CaptureStart {
            capture_id: Uuid::new_v4(),
            meeting_id: Uuid::new_v4(),
            fence: WorkerSecret::random(),
            title: "Synthetic Soniox permission".into(),
            app: None,
            started_at: Utc::now(),
            participants: vec![],
            provider: CaptureSelection {
                provider: CaptureProvider::Soniox,
                model: Some(settings.soniox_model.clone()),
                language: settings.language.clone(),
            },
        };
        let lease = store
            .service_prepare_capture(42, epoch, &proof, &start)
            .unwrap();
        store
            .service_begin_capture(42, epoch, &proof, &lease, &start.fence)
            .unwrap();
        Fixture {
            store,
            settings,
            proof,
            lease,
            fence: start.fence,
        }
    }

    impl Fixture {
        fn call(&self, action: SonioxPermissionAction) -> DataResult<SonioxPermissionReply> {
            self.store.service_soniox_permission(
                42,
                self.proof.library_epoch,
                &self.proof,
                &self.lease,
                &self.fence,
                action,
            )
        }

        fn issue(&self) -> SonioxPermissionPermit {
            let reply = self.call(SonioxPermissionAction::Issue).unwrap();
            assert!(!format!("{reply:?}").contains(SYNTHETIC));
            let SonioxPermissionReply::Issued { permit, credential } = reply else {
                panic!("expected Soniox permit");
            };
            assert_eq!(credential.expose(), SYNTHETIC);
            permit
        }
    }

    #[test]
    fn credential_and_consent_aba_revoke_and_never_readmit_the_same_capture() {
        for change_key in [false, true] {
            let f = fixture();
            let first = f.issue();
            let retry = f.issue();
            assert_eq!(retry.permission_epoch, first.permission_epoch);
            assert!(retry.valid_for_ms <= first.valid_for_ms);
            let mut updated = f.settings.clone();
            updated.user_name = "Unrelated setting".into();
            f.store.set_setting("settings", &updated).unwrap();
            assert!(matches!(
                f.call(SonioxPermissionAction::Renew(first.permission_epoch))
                    .unwrap(),
                SonioxPermissionReply::Renewed { .. }
            ));
            if change_key {
                f.store.set_setting(KEY, &"").unwrap();
                f.store.set_setting(KEY, &SYNTHETIC).unwrap();
            } else {
                let mut revoked = updated.clone();
                revoked.stt_provider = NativeSttProvider::Local;
                revoked.soniox_consent = false;
                f.store.set_setting("settings", &revoked).unwrap();
                f.store.set_setting("settings", &updated).unwrap();
            }
            assert_eq!(
                f.call(SonioxPermissionAction::Renew(first.permission_epoch))
                    .unwrap(),
                SonioxPermissionReply::Stop {
                    permission_epoch: first.permission_epoch,
                    reason: SonioxIncompleteReason::PermissionRevoked,
                }
            );
            assert!(matches!(
                f.call(SonioxPermissionAction::Issue),
                Err(DataError::Forbidden)
            ));
            for _ in 0..2 {
                assert_eq!(
                    f.call(SonioxPermissionAction::AckDrained(first.permission_epoch))
                        .unwrap(),
                    SonioxPermissionReply::Drained {
                        permission_epoch: first.permission_epoch
                    }
                );
            }
            assert!(matches!(
                f.call(SonioxPermissionAction::AckDrained(Uuid::new_v4())),
                Err(DataError::Forbidden)
            ));
        }
    }

    #[test]
    fn wrong_pid_fence_and_expired_permits_do_not_return_a_credential() {
        let f = fixture();
        let attempt = |pid, fence| {
            f.store.service_soniox_permission(
                pid,
                f.proof.library_epoch,
                &f.proof,
                &f.lease,
                fence,
                SonioxPermissionAction::Issue,
            )
        };
        assert!(matches!(attempt(43, &f.fence), Err(DataError::Forbidden)));
        assert!(matches!(
            attempt(42, &WorkerSecret::random()),
            Err(DataError::Forbidden)
        ));
        let initial = f.issue();
        f.store
            .conn
            .lock()
            .execute(
                "UPDATE service_soniox_permissions SET expires_at=?1",
                [Utc::now() - Duration::seconds(1)],
            )
            .unwrap();
        assert_eq!(
            f.call(SonioxPermissionAction::Renew(initial.permission_epoch))
                .unwrap(),
            SonioxPermissionReply::Stop {
                permission_epoch: initial.permission_epoch,
                reason: SonioxIncompleteReason::ControlUnavailable,
            }
        );
        assert!(matches!(
            f.call(SonioxPermissionAction::Issue),
            Err(DataError::Forbidden)
        ));
    }
}

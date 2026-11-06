//! A host owns one gate for the selected repository. External endpoint
//! admission and local admin authentication are separate from this gate.
use bazza_core::{
    data::{DataError, ExternalReadGrant, ExternalReadSecret},
    CoreError, Repository,
};
use bazza_service_api::fixture_read::{FixtureReadReply, FixtureReadRequest};
use std::{future::Future, io, sync::Arc};
use tokio::{
    sync::RwLock,
    time::{timeout_at, Instant},
};
use uuid::Uuid;

#[derive(Debug, PartialEq, Eq)]
pub enum ExternalReadError {
    AccessDenied,
    GrantNotFound,
    DeadlineExceeded,
    BackendUnavailable,
    PublicationFailed,
}

fn core_error(error: CoreError) -> ExternalReadError {
    match error {
        CoreError::Data(DataError::Forbidden) => ExternalReadError::AccessDenied,
        CoreError::Data(DataError::NotFound) => ExternalReadError::GrantNotFound,
        _ => ExternalReadError::BackendUnavailable,
    }
}

pub struct ExternalReadGate {
    repository: Arc<Repository>,
    admission: Arc<RwLock<()>>,
}

impl ExternalReadGate {
    pub fn new(repository: Arc<Repository>) -> Self {
        Self {
            repository,
            admission: Arc::new(RwLock::new(())),
        }
    }

    /// Recheck the grant for this request and retain the shared permit until
    /// the publisher returns. A timed out or failed framed reply must close
    /// its stream, as it may have published only part of the frame.
    pub async fn read<F: Future<Output = io::Result<()>>>(
        &self,
        connection_id: Uuid,
        secret: Arc<ExternalReadSecret>,
        request: FixtureReadRequest,
        deadline: Instant,
        publish: impl FnOnce(FixtureReadReply) -> F,
    ) -> Result<(), ExternalReadError> {
        self.authorized(
            connection_id,
            secret,
            deadline,
            move |repository| repository.fixture_read(request),
            publish,
        )
        .await
    }

    pub(crate) async fn authenticate<F: Future<Output = io::Result<()>>>(
        &self,
        connection_id: Uuid,
        secret: Arc<ExternalReadSecret>,
        deadline: Instant,
        publish: impl FnOnce() -> F,
    ) -> Result<(), ExternalReadError> {
        self.authorized(connection_id, secret, deadline, |_| (), |()| publish())
            .await
    }

    async fn authorized<T: Send + 'static, F: Future<Output = io::Result<()>>>(
        &self,
        connection_id: Uuid,
        secret: Arc<ExternalReadSecret>,
        deadline: Instant,
        project: impl FnOnce(&Repository) -> T + Send + 'static,
        publish: impl FnOnce(T) -> F,
    ) -> Result<(), ExternalReadError> {
        let _permit = timeout_at(deadline, self.admission.read())
            .await
            .map_err(|_| ExternalReadError::DeadlineExceeded)?;
        if Instant::now() >= deadline {
            return Err(ExternalReadError::DeadlineExceeded);
        }
        let repository = self.repository.clone();
        let projection = tokio::task::spawn_blocking(move || {
            repository
                .authorize_external_read(connection_id, &secret)
                .map_err(core_error)?;
            Ok::<_, ExternalReadError>(project(&repository))
        });
        let reply = timeout_at(deadline, projection)
            .await
            .map_err(|_| ExternalReadError::DeadlineExceeded)?
            .map_err(|_| ExternalReadError::BackendUnavailable)??;
        if Instant::now() >= deadline {
            return Err(ExternalReadError::DeadlineExceeded);
        }
        timeout_at(deadline, publish(reply))
            .await
            .map_err(|_| ExternalReadError::DeadlineExceeded)?
            .map_err(|_| ExternalReadError::PublicationFailed)
    }

    /// Call only from the authenticated administrative route. Once the
    /// transaction starts, the owned permit lives with that transaction even
    /// if the client disconnects. A lost acknowledgement can be retried.
    pub async fn revoke(
        &self,
        connection_id: Uuid,
        deadline: Instant,
    ) -> Result<ExternalReadGrant, ExternalReadError> {
        let permit = timeout_at(deadline, self.admission.clone().write_owned())
            .await
            .map_err(|_| ExternalReadError::DeadlineExceeded)?;
        let repository = self.repository.clone();
        let revoke = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            if Instant::now() >= deadline {
                return Err(ExternalReadError::DeadlineExceeded);
            }
            repository
                .revoke_external_read_grant(connection_id)
                .map_err(core_error)
        });
        timeout_at(deadline, revoke)
            .await
            .map_err(|_| ExternalReadError::DeadlineExceeded)?
            .map_err(|_| ExternalReadError::BackendUnavailable)?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bazza_core::MigrationPolicy;
    use bazza_service_api::fixture_read::FixtureReadTool;
    use std::time::Duration;
    use tokio::sync::oneshot;

    fn fixture() -> (
        tempfile::TempDir,
        Arc<ExternalReadGate>,
        Uuid,
        Arc<ExternalReadSecret>,
    ) {
        let directory = tempfile::tempdir().unwrap();
        let fixture = bazza_service_test_support::create_v8(&directory.path().join("mcp")).unwrap();
        let repository =
            Arc::new(Repository::open_fixture(&fixture.root, MigrationPolicy::Upgrade).unwrap());
        let issued = repository
            .issue_external_read_grant("Synthetic test host")
            .unwrap();
        (
            directory,
            Arc::new(ExternalReadGate::new(repository)),
            issued.grant.connection_id,
            Arc::new(issued.secret),
        )
    }

    fn status() -> FixtureReadRequest {
        FixtureReadRequest {
            tool: FixtureReadTool::Status,
            arguments: serde_json::json!({}),
        }
    }

    #[tokio::test]
    async fn queued_revoke_waits_for_publication_and_denies_the_next_read() {
        let (_directory, gate, connection_id, secret) = fixture();
        let (entered, publishing) = oneshot::channel();
        let (release, released) = oneshot::channel();
        let deadline = Instant::now() + Duration::from_secs(5);
        let active = tokio::spawn({
            let gate = gate.clone();
            let secret = secret.clone();
            async move {
                gate.read(
                    connection_id,
                    secret,
                    status(),
                    deadline,
                    |reply| async move {
                        assert!(reply.output.get("data").is_some());
                        entered.send(()).unwrap();
                        released.await.unwrap();
                        Ok(())
                    },
                )
                .await
            }
        });
        publishing.await.unwrap();
        let revoke = gate.revoke(connection_id, deadline);
        tokio::pin!(revoke);
        tokio::select! {
            biased;
            result = &mut revoke => panic!("revoked during authorized publication: {result:?}"),
            () = std::future::ready(()) => {}
        }
        release.send(()).unwrap();
        active.await.unwrap().unwrap();
        assert!(revoke.await.unwrap().revoked_at.is_some());
        assert_eq!(
            gate.read(connection_id, secret, status(), deadline, |_| async {
                panic!("revoked credential reached publication");
            })
            .await,
            Err(ExternalReadError::AccessDenied)
        );
    }

    #[tokio::test]
    async fn failed_and_timed_out_publication_release_the_gate() {
        let (_directory, gate, connection_id, secret) = fixture();
        assert_eq!(
            gate.read(
                connection_id,
                secret.clone(),
                status(),
                Instant::now() + Duration::from_secs(5),
                |_| async { Err(io::Error::other("synthetic publication failure")) }
            )
            .await,
            Err(ExternalReadError::PublicationFailed)
        );
        assert_eq!(
            gate.read(
                connection_id,
                secret,
                status(),
                Instant::now() + Duration::from_millis(500),
                |_| std::future::pending::<io::Result<()>>()
            )
            .await,
            Err(ExternalReadError::DeadlineExceeded)
        );
        assert!(gate
            .revoke(connection_id, Instant::now() + Duration::from_secs(5))
            .await
            .unwrap()
            .revoked_at
            .is_some());
    }

    #[tokio::test]
    async fn disconnected_admin_cannot_release_a_pending_revoke_permit() {
        let (directory, gate, connection_id, secret) = fixture();
        let blocker = rusqlite::Connection::open(directory.path().join("mcp/bazza.db")).unwrap();
        blocker.execute_batch("BEGIN IMMEDIATE").unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut revoke = Box::pin(gate.revoke(connection_id, deadline));
        tokio::select! {
            biased;
            result = &mut revoke => panic!("revocation did not wait for the database: {result:?}"),
            () = std::future::ready(()) => {}
        }
        assert!(gate.admission.try_read().is_err());
        drop(revoke);
        assert!(gate.admission.try_read().is_err());
        blocker.execute_batch("COMMIT").unwrap();
        assert_eq!(
            gate.read(connection_id, secret, status(), deadline, |_| async {
                panic!("disconnected admin revocation was lost");
            })
            .await,
            Err(ExternalReadError::AccessDenied)
        );
    }
}

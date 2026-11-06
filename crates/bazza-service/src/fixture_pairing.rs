//! Explicit Windows pairing for an already running, opted-in library service.

use anyhow::{bail, Context};
use bazza_service_api::external_read::{
    IssueExternalReadGrant, ListExternalReadGrants, RevokeExternalReadGrant,
};
use bazza_service_client::{
    external_read_credentials, validate_fixture_root, validate_managed_root, ServiceClient,
};
use std::path::{Path, PathBuf};
use uuid::Uuid;

pub async fn issue(root: &Path, client_label: String) -> anyhow::Result<()> {
    issue_for(root, client_label, LibraryKind::Fixture).await
}

pub async fn issue_managed(root: &Path, client_label: String) -> anyhow::Result<()> {
    issue_for(root, client_label, LibraryKind::Managed).await
}

pub async fn revoke(root: &Path, connection_id: Uuid) -> anyhow::Result<()> {
    revoke_for(root, connection_id, LibraryKind::Fixture).await
}

pub async fn revoke_managed(root: &Path, connection_id: Uuid) -> anyhow::Result<()> {
    revoke_for(root, connection_id, LibraryKind::Managed).await
}

pub async fn list(root: &Path, request: ListExternalReadGrants) -> anyhow::Result<()> {
    list_for(root, request, LibraryKind::Fixture).await
}

pub async fn list_managed(root: &Path, request: ListExternalReadGrants) -> anyhow::Result<()> {
    list_for(root, request, LibraryKind::Managed).await
}

#[derive(Clone, Copy)]
enum LibraryKind {
    Fixture,
    Managed,
}

impl LibraryKind {
    fn root(self, root: &Path) -> anyhow::Result<PathBuf> {
        Ok(match self {
            Self::Fixture => validate_fixture_root(root)?,
            Self::Managed => validate_managed_root(root)?.0,
        })
    }

    async fn connect(self, root: &Path) -> anyhow::Result<ServiceClient> {
        Ok(match self {
            Self::Fixture => {
                let mut client = ServiceClient::connect(root).await?;
                if !client.status().await?.development_fixture_only {
                    bail!("Fixture pairing requires a fixture service");
                }
                client
            }
            Self::Managed => ServiceClient::connect_managed(root).await?,
        })
    }
}

async fn issue_for(root: &Path, client_label: String, kind: LibraryKind) -> anyhow::Result<()> {
    if !cfg!(windows) {
        bail!("Read grant pairing requires Windows Credential Manager");
    }
    let root = kind.root(root)?;
    let mut client = kind.connect(&root).await?;
    let grant = client
        .issue_external_read_grant(IssueExternalReadGrant { client_label })
        .await?;
    if let Err(error) =
        external_read_credentials::store(grant.library_id, grant.connection_id, &grant.secret)
    {
        let request = RevokeExternalReadGrant {
            connection_id: grant.connection_id,
        };
        let compensated = async {
            if kind.root(&root)? != root {
                bail!("Library root changed during pairing");
            }
            let mut client = kind.connect(&root).await?;
            if client.status().await?.library_id != grant.library_id {
                bail!("Library identity changed during pairing");
            }
            client.revoke_external_read_grant(request).await?;
            Ok(())
        }
        .await;
        if compensated.is_err() {
            bail!(
                "Credential storage failed for connection {} and the grant could not be revoked",
                grant.connection_id
            );
        }
        return Err(error).context("store local read credential");
    }
    println!(
        "{}",
        serde_json::json!({"library_id": grant.library_id, "connection_id": grant.connection_id})
    );
    Ok(())
}

async fn revoke_for(root: &Path, connection_id: Uuid, kind: LibraryKind) -> anyhow::Result<()> {
    let root = kind.root(root)?;
    let mut client = kind.connect(&root).await?;
    let grant = client
        .revoke_external_read_grant(RevokeExternalReadGrant { connection_id })
        .await?;
    if cfg!(windows) {
        external_read_credentials::remove(grant.library_id, grant.connection_id)
            .context("grant revoked; remove local read credential")?;
    }
    println!(
        "{}",
        serde_json::json!({"library_id": grant.library_id, "connection_id": grant.connection_id})
    );
    Ok(())
}

async fn list_for(
    root: &Path,
    request: ListExternalReadGrants,
    kind: LibraryKind,
) -> anyhow::Result<()> {
    let root = kind.root(root)?;
    let mut client = kind.connect(&root).await?;
    let page = client.list_external_read_grants(request).await?;
    println!("{}", serde_json::to_string(&page)?);
    Ok(())
}

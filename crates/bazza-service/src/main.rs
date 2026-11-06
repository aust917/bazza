#![cfg_attr(windows, windows_subsystem = "windows")]

mod fixture_pairing;

use anyhow::{bail, Context};
use bazza_core::{MigrationPolicy, Repository};
use bazza_service_api::external_read::{ListExternalReadGrants, DEFAULT_EXTERNAL_READ_GRANT_LIMIT};
use bazza_service_client::{broker, ServiceClient};
use std::{path::PathBuf, time::Duration};

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
    if let Err(error) = execute().await {
        eprintln!("bazza-service: {error:#}");
        std::process::exit(1);
    }
}

async fn execute() -> anyhow::Result<()> {
    let mut arguments = std::env::args_os().skip(1);
    let action = arguments
        .next()
        .context("explicit service action required")?;
    if action == "init-library" || action == "upgrade-library" {
        let args: Vec<_> = arguments.collect();
        let [flag, path] = args.as_slice() else {
            bail!("library setup requires --library followed by an absolute path");
        };
        if flag != "--library" {
            bail!("library setup requires --library followed by an absolute path");
        }
        let root = PathBuf::from(path);
        let status = tokio::task::spawn_blocking(move || {
            if action == "init-library" {
                Repository::create_managed(&root)
            } else {
                Repository::upgrade_managed(&root)
            }.map(|repository| repository.status())
        })
        .await??;
        println!("{}", serde_json::to_string(&status)?);
        return Ok(());
    }
    let mut root = None;
    let mut rehearsal = None;
    let mut meeting_id = None;
    let mut client_label = None;
    let mut connection_id = None;
    let mut grant_after = None;
    let mut grant_limit = None;
    let mut idle_timeout = 30_000u64;
    while let Some(argument) = arguments.next() {
        if argument == "--library" {
            if root.is_some() {
                bail!("duplicate --library");
            }
            root = Some(PathBuf::from(
                arguments.next().context("missing library path")?,
            ));
        } else if argument == "--rehearsal" {
            if rehearsal.is_some() {
                bail!("duplicate --rehearsal");
            }
            rehearsal = Some(PathBuf::from(
                arguments.next().context("missing rehearsal destination")?,
            ));
        } else if argument == "--meeting-id" {
            meeting_id = Some(uuid::Uuid::parse_str(
                arguments
                    .next()
                    .context("missing meeting ID")?
                    .to_str()
                    .context("invalid meeting ID")?,
            )?);
        } else if argument == "--client-label"
            && (action == "grant-fixture-read" || action == "grant-library-read")
            && client_label.is_none()
        {
            client_label = Some(
                arguments
                    .next()
                    .context("missing client label")?
                    .into_string()
                    .ok()
                    .context("invalid client label")?,
            );
        } else if argument == "--connection-id"
            && (action == "revoke-fixture-read" || action == "revoke-library-read")
            && connection_id.is_none()
        {
            let id = arguments.next().context("missing connection ID")?;
            connection_id = Some(uuid::Uuid::parse_str(
                id.to_str().context("invalid connection ID")?,
            )?);
        } else if argument == "--idle-timeout-ms" {
            idle_timeout = arguments
                .next()
                .context("missing idle timeout")?
                .to_str()
                .context("invalid idle timeout")?
                .parse()?;
        } else if argument == "--after"
            && (action == "list-fixture-read-grants" || action == "list-library-read-grants")
            && grant_after.is_none()
        {
            let after = arguments.next().context("missing grant cursor")?;
            grant_after = Some(uuid::Uuid::parse_str(
                after.to_str().context("invalid grant cursor")?,
            )?);
        } else if argument == "--limit"
            && (action == "list-fixture-read-grants" || action == "list-library-read-grants")
            && grant_limit.is_none()
        {
            grant_limit = Some(
                arguments
                    .next()
                    .context("missing grant limit")?
                    .to_str()
                    .context("invalid grant limit")?
                    .parse()?,
            );
        } else {
            bail!("unknown argument");
        }
    }
    let root = root.context("explicit --library required; no default library is opened")?;
    if action == "rehearse-adoption" {
        let destination = rehearsal.context("explicit --rehearsal destination required")?;
        let report =
            tokio::task::spawn_blocking(move || bazza_core::rehearse_adoption(&root, &destination))
                .await??;
        println!("{}", serde_json::to_string(&report)?);
        return Ok(());
    }
    if rehearsal.is_some() {
        bail!("--rehearsal requires rehearse-adoption");
    }
    if action == "grant-library-read" {
        fixture_pairing::issue_managed(
            &root,
            client_label.context("explicit --client-label required")?,
        )
        .await?;
    } else if action == "revoke-library-read" {
        fixture_pairing::revoke_managed(
            &root,
            connection_id.context("explicit --connection-id required")?,
        )
        .await?;
    } else if action == "list-library-read-grants" {
        fixture_pairing::list_managed(
            &root,
            ListExternalReadGrants {
                after: grant_after,
                limit: grant_limit.unwrap_or(DEFAULT_EXTERNAL_READ_GRANT_LIMIT),
            },
        )
        .await?;
    } else if action == "grant-fixture-read" {
        fixture_pairing::issue(
            &root,
            client_label.context("explicit --client-label required")?,
        )
        .await?;
    } else if action == "revoke-fixture-read" {
        fixture_pairing::revoke(
            &root,
            connection_id.context("explicit --connection-id required")?,
        )
        .await?;
    } else if action == "list-fixture-read-grants" {
        fixture_pairing::list(
            &root,
            ListExternalReadGrants {
                after: grant_after,
                limit: grant_limit.unwrap_or(DEFAULT_EXTERNAL_READ_GRANT_LIMIT),
            },
        )
        .await?;
    } else if action == "serve-fixture" {
        bazza_service::serve_fixture(&root, Duration::from_millis(idle_timeout)).await?;
    } else if action == "serve-fixture-external" {
        bazza_service::serve_fixture_with_external_read(&root, Duration::from_millis(idle_timeout))
            .await?;
    } else if action == "serve-library" || action == "serve-library-external" {
        if meeting_id.is_some() {
            bail!("managed service accepts only --library and --idle-timeout-ms");
        }
        bazza_service::serve_managed(
            &root,
            Duration::from_millis(idle_timeout),
            action == "serve-library-external",
        )
        .await?;
    } else if action == "migrate-fixture" {
        let repository = Repository::open_fixture(&root, MigrationPolicy::Upgrade)?;
        println!("{}", serde_json::to_string(&repository.status())?);
    } else if action == "register-fixture-broker" {
        broker::register_fixture(&std::env::current_exe()?, &root).await?;
    } else if action == "unregister-fixture-broker" {
        broker::unregister_fixture(&root).await?;
    } else if action == "start-fixture" {
        let mut client = ServiceClient::connect_or_start_fixture(&root).await?;
        println!("{}", serde_json::to_string(&client.status().await?)?);
    } else if action == "status-library" || action == "shutdown-library" {
        if meeting_id.is_some() {
            bail!("managed service status and shutdown accept only --library");
        }
        let mut client = ServiceClient::connect_managed(&root).await?;
        if action == "status-library" {
            println!("{}", serde_json::to_string(&client.status().await?)?);
        } else {
            client.shutdown().await?;
        }
    } else {
        let mut client = ServiceClient::connect(&root).await?;
        if action == "status" {
            println!("{}", serde_json::to_string(&client.status().await?)?);
        } else if action == "desktop-bootstrap" {
            println!(
                "{}",
                serde_json::to_string(&client.desktop_bootstrap().await?)?
            );
        } else if action == "get-meeting" {
            println!(
                "{}",
                serde_json::to_string(
                    &client
                        .meeting(meeting_id.context("--meeting-id required")?)
                        .await?
                )?
            );
        } else if action == "shutdown" {
            client.shutdown().await?;
        } else {
            bail!("unsupported service action");
        }
    }
    Ok(())
}

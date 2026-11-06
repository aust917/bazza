//! Read-only Bazza MCP stdio entry point for an explicit library.

mod projection;
mod protocol;
#[cfg(windows)]
mod codex;
#[cfg(windows)]
mod codex_config;
#[cfg(windows)]
mod claude_config;

use std::{env, path::PathBuf, process::ExitCode, time::Duration};

use projection::FixtureReads;
use uuid::Uuid;

enum Source {
    Fixture,
    FixtureGrant(Uuid),
    ManagedGrant(Uuid),
}

fn main() -> ExitCode {
    let mut arguments = env::args_os().skip(1).peekable();
    #[cfg(windows)]
    if arguments.peek().is_some_and(codex::handles) {
        let action = arguments.next().expect("peeked action");
        let Ok(runtime) = runtime() else {
            return ExitCode::FAILURE;
        };
        let result = runtime.block_on(codex::run(action, arguments));
        runtime.shutdown_timeout(Duration::from_millis(250));
        if let Err(error) = result {
            eprintln!("Bazza MCP: {error}");
            return ExitCode::FAILURE;
        }
        return ExitCode::SUCCESS;
    }
    let (Some(command), Some(flag), Some(root)) =
        (arguments.next(), arguments.next(), arguments.next())
    else {
        return usage();
    };
    if flag != "--library" {
        return usage();
    }
    let source = if command == "serve-fixture" && arguments.next().is_none() {
        Source::Fixture
    } else if command == "serve-fixture-granted" || command == "serve-library" {
        let (Some(flag), Some(connection_id), None) =
            (arguments.next(), arguments.next(), arguments.next())
        else {
            return usage();
        };
        let Some(connection_id) = connection_id
            .to_str()
            .and_then(|value| Uuid::parse_str(value).ok())
            .filter(|id| !id.is_nil())
        else {
            return usage();
        };
        if flag != "--connection-id" {
            return usage();
        }
        if command == "serve-library" {
            Source::ManagedGrant(connection_id)
        } else {
            Source::FixtureGrant(connection_id)
        }
    } else {
        return usage();
    };

    let Ok(runtime) = runtime() else {
        eprintln!("Bazza MCP could not start the stdio runtime.");
        return ExitCode::FAILURE;
    };
    let result = runtime.block_on(async {
        let root = PathBuf::from(root);
        let reads = match source {
            Source::Fixture => FixtureReads::connect(&root).await,
            Source::FixtureGrant(id) => FixtureReads::connect_granted(&root, id).await,
            Source::ManagedGrant(id) => FixtureReads::connect_managed(&root, id).await,
        }
        .map_err(|_| "Bazza MCP could not authorize the selected library connection.")?;
        protocol::serve_stdio(reads)
            .await
            .map_err(|_| "Bazza MCP stdio session could not be established or failed.")
    });
    // The platform's blocking stdio read can outlive a broken output pipe.
    runtime.shutdown_timeout(Duration::from_millis(250));
    if let Err(message) = result {
        eprintln!("{message}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

fn runtime() -> std::io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_current_thread().enable_all().build()
}

fn usage() -> ExitCode {
    eprintln!("Usage: bazza-mcp serve-fixture --library <absolute synthetic fixture path>");
    eprintln!(
        "       bazza-mcp serve-fixture-granted --library <fixture path> --connection-id <UUID>"
    );
    eprintln!(
        "       bazza-mcp serve-library --library <managed library path> --connection-id <UUID>"
    );
    #[cfg(windows)]
    eprintln!("       bazza-mcp install-mcp [--enable-codex] [--enable-claude] | uninstall-mcp");
    ExitCode::from(2)
}

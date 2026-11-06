#![cfg_attr(windows, windows_subsystem = "windows")]

use anyhow::{bail, Context, Result};
use bazza_service_probe::{
    launch, read_status, request_stop, run, run_with_instance, MAX_LIFETIME_MS,
};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

fn main() {
    if let Err(error) = execute() {
        eprintln!("bazza-service-probe: {error:#}");
        std::process::exit(1);
    }
}

fn execute() -> Result<()> {
    let mut args = std::env::args_os().skip(1);
    let action = args
        .next()
        .context("expected run, launch, status or stop")?;
    let mut root = None;
    let mut lifetime_ms = 10_000u64;
    let mut hold_parent = false;
    let mut instance_id = None;
    while let Some(arg) = args.next() {
        if arg == "--fixture" {
            if root.is_some() {
                bail!("duplicate --fixture");
            }
            root = Some(PathBuf::from(
                args.next().context("missing --fixture value")?,
            ));
        } else if arg == "--lifetime-ms" {
            lifetime_ms = args
                .next()
                .context("missing lifetime")?
                .to_str()
                .context("invalid lifetime")?
                .parse()?;
        } else if arg == "--hold-parent" {
            hold_parent = true;
        } else if arg == "--instance-id" {
            if instance_id.is_some() {
                bail!("duplicate --instance-id");
            }
            instance_id = Some(uuid::Uuid::parse_str(
                args.next()
                    .context("missing instance ID")?
                    .to_str()
                    .context("invalid instance ID")?,
            )?);
        } else {
            bail!("unknown argument");
        }
    }
    let root = root.context("explicit --fixture is required; no default library is used")?;
    if lifetime_ms == 0 || lifetime_ms > MAX_LIFETIME_MS {
        bail!("invalid lifetime");
    }
    let lifetime = Duration::from_millis(lifetime_ms);
    if action == "run" {
        match instance_id {
            Some(instance) => run_with_instance(&root, lifetime, instance)?,
            None => run(&root, lifetime)?,
        }
    } else if action == "launch" {
        let mut child = launch(&std::env::current_exe()?, &root, lifetime)?;
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Ok(status) = read_status(&root) {
                if status.process_id == child.id() && !status.finished {
                    println!("{}", serde_json::to_string(&status)?);
                    break;
                }
            }
            if child.try_wait()?.is_some() {
                bail!("DETACHED_CHILD_FAILED");
            }
            if Instant::now() >= deadline {
                // Only terminate our own child; never use a stale status PID.
                let _ = child.kill();
                let _ = child.wait();
                bail!("DETACHED_START_TIMEOUT");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        if hold_parent {
            // Used by the process-tree kill acceptance test, never by the GUI.
            child.wait()?;
        }
    } else if action == "status" {
        println!("{}", serde_json::to_string(&read_status(&root)?)?);
    } else if action == "stop" {
        request_stop(&root)?;
    } else {
        bail!("unknown action");
    }
    Ok(())
}

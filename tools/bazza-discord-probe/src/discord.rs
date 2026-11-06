use std::{path::Path, time::Duration};

use anyhow::{ensure, Context, Result};
use bazza_discord::{VoiceSession, VoiceStatus};

pub async fn record(
    guild: u64,
    channel: u64,
    dir: &Path,
    duration: Duration,
    token: &str,
) -> Result<()> {
    ensure!(!dir.exists(), "output directory already exists");
    let session =
        VoiceSession::start_with_limit(guild, channel, dir.to_owned(), token.to_owned(), duration);
    let mut status = session.subscribe();
    let mut announced = false;
    loop {
        match &*status.borrow_and_update() {
            VoiceStatus::Connecting => {}
            VoiceStatus::Connected | VoiceStatus::Receiving => {
                if !announced {
                    println!("Recording selected Discord voice channel. Ctrl+C stops and saves the tracks.");
                    announced = true;
                }
            }
            VoiceStatus::Stopped | VoiceStatus::Interrupted | VoiceStatus::Failed(_) => break,
        }
        tokio::select! {
            changed = status.changed() => { if changed.is_err() { break; } }
            _ = tokio::signal::ctrl_c() => break,
        }
    }
    let outcome = session.stop().await?;
    if !outcome.disconnected {
        eprintln!("Could not confirm voice disconnect. Closing the gateway will end the session.");
    }
    let manifest = outcome
        .manifest
        .context("stopped before recording started")?;
    let finish_reason = manifest.finish_reason.as_str();
    println!(
        "Saved {} track(s) to {} ({finish_reason})",
        manifest.tracks.len(),
        dir.display()
    );
    ensure!(
        !manifest.tracks.is_empty(),
        "no mapped Discord voice frames were received"
    );
    ensure!(
        matches!(finish_reason, "duration_elapsed" | "stopped_by_operator"),
        "recording ended early: {finish_reason}"
    );
    Ok(())
}

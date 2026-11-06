mod discord;
mod scenario;
mod sender;
#[cfg(feature = "transcribe")]
mod transcribe;

use std::{env, path::Path, time::Duration};

use anyhow::{bail, ensure, Context, Result};
use bazza_discord::tracks;

const USAGE: &str =
    "bazza-discord-probe record <guild-id> <channel-id> <new-output-dir> [duration-seconds]\n\
    bazza-discord-probe validate-scenario <scenario.json>\n\
    bazza-discord-probe send-scenario <guild-id> <channel-id> <scenario.json> <credentials.json|-> <new-report.json>\n\
    bazza-discord-probe transcribe <tracks-dir> <model-file> [language]\n\
    Record reads BAZZA_DISCORD_BOT_TOKEN. Duration is 1–3600 seconds (default 60).\n\
    Send-scenario reads private credentials from a local file or piped stdin (-). Start the receiver separately.\n\
    Transcribe requires the transcribe Cargo feature and a local Whisper model.";

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<_> = env::args_os().skip(1).collect();
    match args.first().and_then(|arg| arg.to_str()) {
        Some("record") if (4..=5).contains(&args.len()) => {
            let guild = snowflake(&args[1], "guild")?;
            let channel = snowflake(&args[2], "channel")?;
            let seconds = match args.get(4) {
                Some(value) => value
                    .to_string_lossy()
                    .parse()
                    .context("invalid duration")?,
                None => 60,
            };
            ensure!(
                (1..=3_600).contains(&seconds),
                "duration must be 1–3600 seconds"
            );
            let token = env::var("BAZZA_DISCORD_BOT_TOKEN")
                .context("set BAZZA_DISCORD_BOT_TOKEN in the process environment")?;
            ensure!(!token.trim().is_empty(), "bot token is empty");
            discord::record(
                guild,
                channel,
                Path::new(&args[3]),
                Duration::from_secs(seconds),
                &token,
            )
            .await
        }
        Some("transcribe") if (3..=4).contains(&args.len()) => {
            #[cfg(feature = "transcribe")]
            {
                let lang = args
                    .get(3)
                    .map(|value| value.to_str().context("invalid language"))
                    .transpose()?;
                transcribe::run(Path::new(&args[1]), Path::new(&args[2]), lang)
            }
            #[cfg(not(feature = "transcribe"))]
            bail!("rebuild with --features transcribe to use this subcommand")
        }
        Some("validate-scenario") if args.len() == 2 => {
            let plan = scenario::load(Path::new(&args[1]))?;
            println!(
                "Validated {} participants, {} utterances and a {} ms scenario.",
                plan.participants.len(),
                plan.utterance_count,
                plan.duration_ms
            );
            Ok(())
        }
        Some("send-scenario") if args.len() == 6 => {
            let guild = snowflake(&args[1], "guild")?;
            let channel = snowflake(&args[2], "channel")?;
            let plan = scenario::load(Path::new(&args[3]))?;
            sender::send(
                guild,
                channel,
                plan,
                Path::new(&args[4]),
                Path::new(&args[5]),
            )
            .await
        }
        Some("-h" | "--help") | None => {
            println!("{USAGE}");
            Ok(())
        }
        _ => bail!("{USAGE}"),
    }
}

fn snowflake(value: &std::ffi::OsStr, label: &str) -> Result<u64> {
    let value = value.to_str().context("Discord ID must be ASCII")?;
    ensure!(
        value.bytes().all(|byte| byte.is_ascii_digit()),
        "invalid {label} ID"
    );
    let id = value
        .parse::<u64>()
        .with_context(|| format!("invalid {label} ID"))?;
    ensure!(id != 0, "invalid {label} ID");
    Ok(id)
}

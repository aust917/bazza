use std::{
    env, fs,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime},
};

use bazza_attendee::{Audit, Config, Phase, RecordingClock, Session};
use serde::Deserialize;

const USAGE: &str = "bazza-ingress-audit --live <fresh-run-dir> <config.json> --confirm-room\n\
                     bazza-ingress-audit --fixture <fresh-run-dir> <config.json>\n\
                     Set BAZZA_AUDIT_ATTENDEE_API_KEY in the process environment.";
const POLL: Duration = Duration::from_millis(50);
const JOIN_TIMEOUT: Duration = Duration::from_secs(205);
const ADOPT_TIMEOUT: Duration = Duration::from_secs(75);
const FINISH_TIMEOUT: Duration = Duration::from_secs(45);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Inputs {
    api_url: String,
    callback_url: String,
    bind_address: String,
    certificate_path: PathBuf,
    private_key_path: PathBuf,
    meeting_url: String,
    max_seconds: Option<u64>,
}

fn flag(directory: &Path, name: &str) -> bool {
    directory.join(name).is_file()
}

fn signal(directory: &Path, name: &str) -> Result<(), &'static str> {
    fs::File::create_new(directory.join(name))
        .map_err(|_| "could not write a coordination signal")?;
    Ok(())
}

fn connected_status(phase: Phase) -> Result<(), &'static str> {
    match phase {
        Phase::Failed => Err("Attendee reported a failed session"),
        Phase::Finished => Err("Attendee left before the requested stop"),
        _ => Ok(()),
    }
}

async fn drive(
    session: &Session,
    directory: &Path,
    max_recording: Duration,
) -> Result<(), &'static str> {
    let deadline = Instant::now() + JOIN_TIMEOUT;
    loop {
        if flag(directory, "stop.signal") {
            return Ok(());
        }
        let phase = session.status().phase;
        connected_status(phase)?;
        if phase == Phase::Connected {
            signal(directory, "connected.signal")?;
            break;
        }
        if Instant::now() >= deadline {
            return Err("admission timed out");
        }
        tokio::time::sleep(POLL).await;
    }

    let deadline = Instant::now() + ADOPT_TIMEOUT;
    loop {
        if flag(directory, "stop.signal") {
            return Ok(());
        }
        connected_status(session.status().phase)?;
        if flag(directory, "adopt.signal") {
            session
                .adopt(
                    directory,
                    RecordingClock::new(Instant::now(), SystemTime::now()),
                )
                .map_err(|_| "could not adopt the connected Attendee session")?;
            signal(directory, "adopted.signal")?;
            break;
        }
        if Instant::now() >= deadline {
            return Err("adoption signal timed out");
        }
        tokio::time::sleep(POLL).await;
    }

    let deadline = Instant::now() + max_recording;
    loop {
        if flag(directory, "stop.signal") || Instant::now() >= deadline {
            return Ok(());
        }
        connected_status(session.status().phase)?;
        tokio::time::sleep(POLL).await;
    }
}

async fn run() -> Result<(), &'static str> {
    let args: Vec<_> = env::args_os().skip(1).collect();
    let (fixture, directory, settings) = match args.as_slice() {
        [mode, directory, settings, confirmation]
            if mode == "--live" && confirmation == "--confirm-room" =>
        {
            (false, PathBuf::from(directory), PathBuf::from(settings))
        }
        [mode, directory, settings] if mode == "--fixture" => {
            (true, PathBuf::from(directory), PathBuf::from(settings))
        }
        _ => return Err(USAGE),
    };
    if !directory.is_absolute() || !settings.is_absolute() {
        return Err("use absolute paths for the new run directory and config");
    }
    let inputs: Inputs =
        serde_json::from_slice(&fs::read(settings).map_err(|_| "could not read audit config")?)
            .map_err(|_| "invalid audit config")?;
    let api_key = env::var("BAZZA_AUDIT_ATTENDEE_API_KEY")
        .map_err(|_| "BAZZA_AUDIT_ATTENDEE_API_KEY is missing")?;
    let bind_address: std::net::SocketAddr = inputs
        .bind_address
        .parse()
        .map_err(|_| "invalid bind_address")?;
    if fixture {
        let api = reqwest::Url::parse(&inputs.api_url).map_err(|_| "invalid fixture API URL")?;
        let callback =
            reqwest::Url::parse(&inputs.callback_url).map_err(|_| "invalid fixture callback")?;
        if api.scheme() != "http"
            || api.host_str() != Some("127.0.0.1")
            || callback.host_str() != Some("127.0.0.1")
            || !bind_address.ip().is_loopback()
            || inputs.meeting_url != "https://meet.google.com/abc-defg-hij"
            || api_key != "fixture-local-secret"
        {
            return Err("fixture mode requires the dummy room and key with loopback endpoints");
        }
    }
    let seconds = inputs.max_seconds.unwrap_or(600);
    if !(1..=3600).contains(&seconds) {
        return Err("max_seconds must be between 1 and 3600");
    }
    let config = Config {
        api_url: inputs.api_url,
        api_key,
        callback_url: inputs.callback_url,
        bind_address,
        certificate_path: inputs.certificate_path,
        private_key_path: inputs.private_key_path,
    };
    config
        .validate()
        .map_err(|_| "Attendee config validation failed")?;
    let meeting_url = bazza_attendee::normalize_meeting_url(&inputs.meeting_url)
        .map_err(|_| "invalid test meeting URL")?;
    fs::create_dir(&directory).map_err(|_| "run directory must be a new directory")?;
    let (audit, log) = Audit::create(&directory.join("ingress.jsonl"))
        .map_err(|_| "could not create ingress audit")?;
    let phase_audit = audit.clone();
    let session = Session::prepare_audited(config, meeting_url, audit.clone(), move |status| {
        phase_audit.mark(match status.phase {
            Phase::Connecting => "phase_connecting",
            Phase::WaitingAdmission => "phase_waiting_admission",
            Phase::Connected => "phase_connected",
            Phase::Waiting => "phase_waiting_pcm",
            Phase::Receiving => "phase_receiving",
            Phase::Failed => "phase_failed",
            Phase::Finished => "phase_finished",
        });
    });
    let result = match session {
        Ok(session) => {
            let driven = drive(&session, &directory, Duration::from_secs(seconds)).await;
            let finished = match tokio::time::timeout(FINISH_TIMEOUT, session.finish()).await {
                Ok(Ok(())) => {
                    audit.mark("cleanup_confirmed");
                    Ok(())
                }
                _ => {
                    audit.mark("cleanup_unconfirmed");
                    Err("Attendee cleanup could not be confirmed")
                }
            };
            finished.and(driven)
        }
        Err(_) => {
            audit.mark("prepare_failed");
            Err("could not prepare the Attendee listener")
        }
    };
    log.finish()
        .map_err(|_| "audit output could not be finalized")?;
    result
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    if env::args_os().nth(1).as_deref() == Some(std::ffi::OsStr::new("--help")) {
        println!("{USAGE}");
        return;
    }
    if let Err(error) = run().await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

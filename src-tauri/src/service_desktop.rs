//! Run ordinary desktop capture windows against an explicit service library.
use crate::{commands, recorder_service::ServiceCapture, state::RuntimeState};
use bazza_service_client::{validate_fixture_root, validate_managed_root, RepositoryClient};
use std::{ffi::OsString, path::PathBuf};
use tauri::{Manager, WebviewUrl, WindowEvent};

const FLAG: &str = "--service-desktop";
const LIBRARY_FLAG: &str = "--service-library";
const IDENTIFIER: &str = "app.bazza.service.desktop";

pub(crate) fn selected_root(args: &[OsString]) -> Result<Option<PathBuf>, &'static str> {
    if !args
        .iter()
        .any(|arg| arg.to_string_lossy().starts_with(FLAG))
    {
        return Ok(None);
    }
    let [flag, root] = args else {
        return Err("Use --service-desktop followed by one absolute synthetic library directory.");
    };
    if flag != FLAG {
        return Err("Use --service-desktop followed by one absolute synthetic library directory.");
    }
    validate_fixture_root(&PathBuf::from(root))
        .map(Some)
        .map_err(|_| "The selected directory is not a valid synthetic library.")
}

pub(crate) fn selected_library(args: &[OsString]) -> Result<Option<PathBuf>, &'static str> {
    if !args.iter().any(|arg| arg.to_string_lossy().starts_with(LIBRARY_FLAG)) {
        return Ok(None);
    }
    let [flag, root] = args else {
        return Err("Use --service-library followed by one absolute managed library directory.");
    };
    if flag != LIBRARY_FLAG {
        return Err("Use --service-library followed by one absolute managed library directory.");
    }
    validate_managed_root(&PathBuf::from(root))
        .map(|(root, _)| Some(root))
        .map_err(|_| "The selected managed library has not been initialized.")
}

pub(crate) fn installed_library() -> Result<Option<PathBuf>, &'static str> {
    #[cfg(windows)]
    {
        use bazza_service_client::installed::{default_library, Install};
        let Some(root) = default_library() else {
            return Ok(None);
        };
        let adapter = std::env::current_exe()
            .map_err(|_| "Could not locate the installed Bazza application.")?
            .with_file_name("bazza-mcp.exe");
        if adapter.is_file()
            && Install::load(&root)
                .map_err(|_| "The Bazza installation library is invalid.")?
                .is_some_and(|install| install.installed && install.adapter == adapter)
        {
            return Ok(Some(root));
        }
    }
    Ok(None)
}

pub(crate) fn run(root: PathBuf, managed: bool, installed: bool) -> Result<(), &'static str> {
    let (repository, runtime, identity) = tauri::async_runtime::block_on(async {
        let repository = if installed {
            RepositoryClient::connect_or_start_managed(&root).await?
        } else if managed {
            RepositoryClient::connect_managed(&root).await?
        } else {
            RepositoryClient::connect(&root).await?
        };
        let runtime = RuntimeState::initialize_from_service(&repository).await?;
        let identity = repository.identity().await;
        Ok::<_, bazza_service_client::ClientError>((repository, runtime, identity))
    })
    .map_err(|_| "Could not initialize the desktop from the selected service library.")?;

    let mut context = tauri::generate_context!();
    let config = context.config_mut();
    if !installed {
        config.identifier = IDENTIFIER.into();
        config.product_name = Some("Bazza Service Desktop".into());
    }
    config.app.security.asset_protocol = Default::default();
    for window in &mut config.app.windows {
        let url = match window.label.as_str() {
            "main" => {
                if installed {
                    "index.html?mode=service-desktop&managed=1&installed=1"
                } else if managed {
                    window.title = "Bazza Service Desktop".into();
                    "index.html?mode=service-desktop&managed=1"
                } else {
                    window.title = "Bazza Service Desktop".into();
                    "index.html?mode=service-desktop"
                }
            }
            "popup" => "index.html?mode=service-desktop&window=popup",
            "recordbar" => "index.html?mode=service-desktop&window=recordbar",
            _ => return Err("Unexpected desktop window configuration."),
        };
        window.url = WebviewUrl::App(url.into());
        window.incognito = true;
    }

    let mut builder = tauri::Builder::default();
    if installed {
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, _, _| {
            crate::windows::focus_main(app);
        }));
    }
    builder
        .plugin(
            tauri_plugin_log::Builder::new()
                .targets([tauri_plugin_log::Target::new(
                    tauri_plugin_log::TargetKind::LogDir { file_name: None },
                )])
                .level(log::LevelFilter::Info)
                .build(),
        )
        .plugin(tauri_plugin_opener::init())
        .manage(repository)
        .manage(runtime)
        .manage(commands::settings::ServiceSettingsOperations::default())
        .manage(ServiceCapture::for_root(root.clone(), identity.library_id, managed))
        .manage(commands::meetings::ServicePlayback::default())
        .manage(crate::windows::RecordingSetupNavigation::default())
        .manage(crate::bot_connection::BotConnections::default())
        .manage(crate::bot_transcript::Jobs::default())
        .manage(crate::transcript_versions::VersionJobs::default())
        .manage(crate::subscription::SubscriptionJobs::default())
        .manage(commands::service_summary::ServiceSummaryJobs::default())
        .on_page_load(crate::windows::recording_setup_page_load)
        .register_asynchronous_uri_scheme_protocol(
            "bazza-media",
            commands::meetings::serve_service_playback,
        )
        .setup(move |app| {
            let worker = crate::service_postprocess::start(
                app.handle().clone(), root, identity.library_id, managed,
            ).map_err(std::io::Error::other)?;
            app.manage(worker);
            let repository = app.state::<RepositoryClient>().inner().clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    let _ = repository.keep_desktop_session().await;
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::meetings::list_meetings,
            commands::organize::list_meetings_filtered,
            commands::service_library::get_service_library_organization,
            commands::service_library::mutate_service_library_organization,
            commands::meetings::get_meeting,
            commands::service_summary::get_service_summary,
            commands::service_summary::prepare_service_summary,
            commands::service_summary::get_service_summary_job,
            commands::service_summary::generate_service_summary,
            commands::service_summary::cancel_service_summary,
            crate::subscription::subscription_agents,
            crate::subscription::get_subscription_job,
            crate::subscription::start_subscription,
            crate::subscription::cancel_subscription,
            commands::meetings::release_meeting_media,
            commands::meetings::rename_meeting,
            commands::meetings::delete_meeting,
            commands::recording::start_recording,
            commands::recording::connect_recording_bot,
            commands::recording::disconnect_recording_bot,
            commands::recording::stop_recording,
            commands::recording::pause_recording,
            commands::recording::resume_recording,
            commands::recording::recording_status,
            commands::recording::discord_recording_status,
            commands::recording::attendee_recording_status,
            commands::recording::live_snapshot,
            commands::recording::open_record_picker,
            commands::recording::dismiss_detection,
            commands::recording::open_main,
            commands::settings::get_settings,
            commands::settings::set_settings,
            commands::settings::get_ui_locale,
            crate::discord_settings::discord_status,
            crate::discord_settings::discord_open_developer_portal,
            crate::attendee_settings::attendee_status,
            crate::attendee_settings::attendee_setup,
            crate::attendee_settings::attendee_configure,
            crate::attendee_settings::attendee_disconnect,
            crate::attendee_tracks::attendee_tracks,
            crate::attendee_tracks::attendee_open_track,
            crate::bot_transcript::bot_transcript_sources,
            crate::bot_transcript::bot_transcript_jobs,
            crate::bot_transcript::cancel_bot_transcript,
            crate::bot_transcript::transcribe_bot_tracks,
            crate::transcript_versions::transcript_versions,
            crate::transcript_versions::transcript_version,
            crate::transcript_versions::regenerate_transcript,
            crate::transcript_versions::transcript_version_job,
            crate::transcript_versions::cancel_transcript_version,
            crate::discord_settings::discord_save_token,
            crate::discord_settings::discord_disconnect,
            crate::discord_settings::discord_guilds,
            crate::discord_settings::discord_channels,
            crate::discord_settings::discord_set_channel,
            crate::discord_settings::discord_invite_url,
            commands::codex_mcp::get_codex_mcp_status,
            commands::codex_mcp::set_codex_mcp_enabled,
            commands::codex_mcp::get_claude_mcp_status,
            commands::codex_mcp::set_claude_mcp_enabled,
            crate::stt_stage::soniox_key_status,
            crate::stt_stage::save_soniox_key,
            commands::models::probe_hardware,
            commands::models::list_models,
            commands::models::download_model,
            commands::models::cancel_download,
            commands::models::remove_model,
            commands::devices::list_audio_devices,
            commands::devices::list_video_sources,
        ])
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                if window.label() == "main" {
                    crate::bot_connection::cancel_pending(window.app_handle());
                    if window.state::<ServiceCapture>().may_close() {
                        window.state::<crate::service_postprocess::ServicePostprocess>().shutdown();
                        crate::bot_connection::exit_after_pending(window.app_handle());
                    }
                } else {
                    let _ = window.hide();
                }
            }
        })
        .build(context)
        .map_err(|_| "The service desktop windows could not be started.")?
        .run(|app, event| {
            if matches!(&event, tauri::RunEvent::Exit) {
                app.state::<commands::service_summary::ServiceSummaryJobs>().shutdown();
            }
            crate::subscription::on_app_event(app, event);
        });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_desktop_rejects_mixed_missing_and_unmarked_roots() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().as_os_str().to_os_string();
        assert!(selected_root(&[]).unwrap().is_none());
        assert!(selected_root(&[FLAG.into()]).is_err());
        assert!(selected_root(&["--service-desktop-extra".into(), path.clone()]).is_err());
        assert!(selected_root(&[FLAG.into(), path.clone()]).is_err());
        std::fs::write(
            root.path().join(".bazza-synthetic-fixture"),
            "bazza-mcp-v8-fixture-v1\n",
        )
        .unwrap();
        assert!(selected_root(&[FLAG.into(), path.clone(), "--service-fixture".into()]).is_err());
        assert_eq!(
            selected_root(&[FLAG.into(), path]).unwrap(),
            Some(root.path().canonicalize().unwrap())
        );
    }
}

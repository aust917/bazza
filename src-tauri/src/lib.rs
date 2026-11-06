mod attendee_managed;
mod attendee_recording;
mod attendee_settings;
mod attendee_tracks;
mod bot_connection;
mod bot_transcript;
mod bot_supplement;
#[cfg(test)]
#[path = "bot_supplement_model_tests.rs"]
mod bot_supplement_model_tests;
mod calendar;
mod commands;
mod detector_loop;
mod discord_recording;
mod discord_settings;
mod embed_stage;
mod events;
mod integration_credentials;
mod pipeline;
mod processing_jobs;
mod recorder;
mod recorder_service;
mod settings;
mod shortcuts;
mod service_desktop;
mod service_fixture;
mod service_postprocess;
mod asr_pick;
mod import;
mod live_feed;
mod managed_transcript_versions;
mod participant_live;
mod smoke;
mod state;
mod startup;
mod summary_stage;
mod subscription;
mod stt_stage;
mod transcript_versions;
pub mod stt_handoff;
mod tray;
mod webhook;
mod windows;
mod ui_locale;
mod ui_message;
mod video_diagnostic;

use state::AppState;
use tauri::{Manager, WindowEvent};

// The library test executable can link TaskDialogIndirect through Tauri. It
// does not receive the app resources or the integration-test linker flags.
// Ask MSVC for Common Controls v6 in this test executable's sidecar manifest.
#[cfg(all(test, windows, target_env = "msvc"))]
#[used]
#[unsafe(link_section = ".drectve")]
static WINDOWS_UNIT_TEST_MANIFEST: [u8; 168] = *b" /MANIFESTDEPENDENCY:\"type='win32' name='Microsoft.Windows.Common-Controls' version='6.0.0.0' processorArchitecture='*' publicKeyToken='6595b64144ccf1df' language='*'\" ";

pub fn run() {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let portable = matches!(args.as_slice(), [arg] if arg == "--portable");
    if portable && std::env::var_os("BAZZA_DATA_DIR").is_none() {
        eprintln!("Portable mode requires an explicit BAZZA_DATA_DIR.");
        std::process::exit(2);
    }
    let service_selection = service_desktop::selected_library(&args).and_then(|selected| {
        if let Some(root) = selected {
            return Ok(Some((root, true, false)));
        }
        if args.is_empty() {
            if let Some(root) = service_desktop::installed_library()? {
                return Ok(Some((root, true, true)));
            }
        }
        service_desktop::selected_root(&args).map(|selected| selected.map(|root| (root, false, false)))
    });
    match service_selection {
        Ok(Some((root, managed, installed))) => {
            if let Err(error) = service_desktop::run(root, managed, installed) {
                eprintln!("Bazza service desktop: {error}");
                std::process::exit(2);
            }
            return;
        }
        Ok(None) => {}
        Err(error) => {
            eprintln!("Bazza service desktop: {error}");
            std::process::exit(2);
        }
    }
    match service_fixture::selected_root(args) {
        Ok(Some(root)) => {
            if let Err(error) = service_fixture::run(root) {
                eprintln!("Bazza service fixture: {}", error.message);
                std::process::exit(2);
            }
            return;
        }
        Ok(None) => {}
        Err(error) => {
            eprintln!("Bazza service fixture: {}", error.message);
            std::process::exit(2);
        }
    }
    tauri::Builder::default()
        .plugin(tauri_plugin_log::Builder::new().level(log::LevelFilter::Info).build())
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| windows::focus_main(app)))
        .plugin(startup::library_state_plugin(move |app| {
            // Plugin admission precedes configured webviews. State is ready for
            // their first IPC, and duplicate launches never open the database.
            let data_dir = bazza_store::data_dir();
            let store = bazza_store::Store::open_legacy(&bazza_store::database_path(&data_dir))?;
            if portable {
                settings::Settings::initialize_portable(&store)?;
            }
            video_diagnostic::restore_saved(&store);
            app.manage(AppState::new(store));
            Ok(())
        }))
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_os::init())
        .plugin(tauri_plugin_process::init())
        .manage(windows::RecordingSetupNavigation::default())
        .manage(bot_connection::BotConnections::default())
        .manage(bot_transcript::Jobs::default())
        .manage(transcript_versions::VersionJobs::default())
        .manage(subscription::SubscriptionJobs::default())
        .on_page_load(windows::recording_setup_page_load)
        .setup(|app| {
            let handle = app.handle().clone();
            tray::build(&handle)?;
            windows::refresh_titles(&handle);
            let hotkey = handle.state::<AppState>().settings.read().hotkey.clone();
            if let Err(e) = shortcuts::register(&handle, &hotkey) {
                log::warn!("hotkey: {e}");
            }
            recorder::recover_orphans(&handle);
            detector_loop::spawn(handle.clone());
            calendar::spawn(handle.clone());
            spawn_engine_idle_unloader(handle.clone());
            {
                let st = handle.state::<AppState>();
                let any_share = st.store.list_shares().map(|s| s.iter().any(|x| x.enabled)).unwrap_or(false);
                if any_share {
                    let port = st.settings.read().share_port;
                    match bazza_share::ShareServer::start(st.store.clone(), st.store.root().join("clips"), port) {
                        Ok(h) => *st.share_server.lock() = Some(h),
                        Err(e) => log::warn!("share server: {e}"),
                    }
                }
            }
            // The player streams mix.wav / screen.mp4 through the asset protocol; the
            // config-file scope cannot know a user-chosen data folder, so allow it here.
            let recordings = bazza_store::data_dir().join("recordings");
            if let Err(e) = handle.asset_protocol_scope().allow_directory(&recordings, true) {
                log::warn!("asset scope: {e}");
            }
            let samples = bazza_store::data_dir().join("voice-samples");
            if let Err(e) = handle.asset_protocol_scope().allow_directory(&samples, true) {
                log::warn!("voice sample asset scope: {e}");
            }
            log::info!("Bazza started; data dir {}", bazza_store::data_dir().display());
            if handle.state::<AppState>().ffmpeg.is_none() {
                log::warn!("ffmpeg sidecar not found; screen video disabled");
            }
            if std::env::var_os("BAZZA_SMOKE").is_some() {
                smoke::run(handle);
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                let app = window.app_handle();
                if window.label() == "main" {
                    bot_connection::cancel_pending(app);
                }
                let close_to_tray = app.state::<AppState>().settings.read().close_to_tray;
                // Popup/record bar always hide; the main window hides when close-to-tray is on.
                if window.label() != "main" || close_to_tray {
                    let _ = window.hide();
                    api.prevent_close();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::meetings::list_meetings,
            commands::meetings::get_meeting,
            commands::meetings::set_transcript_correction,
            commands::meetings::rename_meeting,
            commands::meetings::delete_meeting,
            commands::meetings::search,
            commands::meetings::retranscribe,
            transcript_versions::transcript_versions,
            transcript_versions::transcript_version,
            transcript_versions::regenerate_transcript,
            transcript_versions::transcript_version_job,
            transcript_versions::cancel_transcript_version,
            commands::meetings::list_upcoming,
            commands::meetings::refresh_calendar,
            commands::meetings::rediarize,
            commands::meetings::reembed,
            commands::meetings::import_recording,
            commands::recording::start_recording,
            commands::recording::connect_recording_bot,
            commands::recording::disconnect_recording_bot,
            commands::recording::stop_recording,
            commands::recording::pause_recording,
            commands::recording::resume_recording,
            commands::recording::mark_highlight,
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
            commands::settings::set_ui_locale,
            discord_settings::discord_status,
            discord_settings::discord_open_developer_portal,
            attendee_settings::attendee_status,
            attendee_settings::attendee_setup,
            attendee_settings::attendee_configure,
            attendee_settings::attendee_disconnect,
            attendee_tracks::attendee_tracks,
            attendee_tracks::attendee_open_track,
            bot_transcript::bot_transcript_sources,
            bot_transcript::bot_transcript_jobs,
            bot_transcript::cancel_bot_transcript,
            bot_transcript::transcribe_bot_tracks,
            discord_settings::discord_save_token,
            discord_settings::discord_disconnect,
            discord_settings::discord_guilds,
            discord_settings::discord_channels,
            discord_settings::discord_set_channel,
            discord_settings::discord_invite_url,
            stt_stage::soniox_key_status,
            stt_stage::save_soniox_key,
            stt_stage::stt_transcript,
            commands::settings::data_info,
            commands::settings::test_webhook,
            commands::models::probe_hardware,
            commands::models::list_models,
            commands::models::download_model,
            commands::models::cancel_download,
            commands::models::remove_model,
            commands::speakers::meeting_speakers,
            commands::speakers::rename_speaker,
            commands::speakers::accept_speaker_suggestion,
            commands::speakers::list_known_speakers,
            commands::speakers::delete_known_speaker,
            commands::speakers::list_voice_profiles,
            commands::speakers::create_voice_profile,
            commands::speakers::rename_voice_profile,
            commands::speakers::list_voice_samples,
            commands::speakers::add_voice_sample,
            commands::speakers::delete_voice_sample,
            commands::speakers::recompute_voice_profile,
            commands::speakers::voice_sample_audio,
            commands::speakers::rerun_cleanup,
            commands::speakers::test_llm_endpoint,
            commands::ai::list_templates,
            commands::ai::save_template,
            commands::ai::delete_template,
            commands::ai::get_summary,
            commands::ai::generate_summary,
            commands::ai::summary_agents,
            crate::subscription::subscription_agents,
            crate::subscription::get_subscription_job,
            crate::subscription::start_subscription,
            crate::subscription::cancel_subscription,
            commands::ai::summary_job,
            commands::ai::cancel_summary,
            commands::ai::draft_followup,
            commands::ai::list_chats,
            commands::ai::create_chat,
            commands::ai::delete_chat,
            commands::ai::chat_messages,
            commands::ai::send_chat,
            commands::ai::cancel_chat,
            commands::organize::list_folders,
            commands::organize::create_folder,
            commands::organize::update_folder,
            commands::organize::delete_folder,
            commands::organize::move_meeting,
            commands::organize::list_meetings_filtered,
            commands::organize::list_tags,
            commands::organize::meeting_tags,
            commands::organize::tag_meeting,
            commands::organize::untag_meeting,
            commands::organize::export_clip,
            commands::organize::create_share,
            commands::organize::list_shares,
            commands::organize::set_share_enabled,
            commands::organize::delete_share,
            commands::organize::export_meetings,
            commands::organize::import_meetings,
            commands::organize::export_html,
            commands::organize::save_text_file,
            commands::organize::change_data_dir,
            commands::devices::list_audio_devices,
            commands::devices::list_video_sources,
            commands::devices::sample_levels,
        ])
        .build(tauri::generate_context!())
        .expect("error while running Bazza")
        .run(subscription::on_app_event);
}

/// Free model memory after 10 minutes without use (never while recording).
fn spawn_engine_idle_unloader(app: tauri::AppHandle) {
    std::thread::spawn(move || loop {
        std::thread::sleep(std::time::Duration::from_secs(60));
        let state = app.state::<AppState>();
        if state.recording.lock().is_some() || state.busy.load(std::sync::atomic::Ordering::SeqCst) > 0 {
            continue;
        }
        let idle = state.engines.lock().last_used.map(|t| t.elapsed() > std::time::Duration::from_secs(600)).unwrap_or(false);
        if idle {
            log::info!("unloading idle AI engines");
            state.unload_engines();
        }
    });
}

//! Deterministic scheduling contracts; these tests never open devices or a user library.
use super::{blocking, CmdResult};
use std::future::Future;
use std::sync::mpsc;
use std::task::{Context, Poll, Waker};
use std::time::Duration;

#[test]
fn performance_blocking_work_yields_the_caller_and_allows_an_unrelated_command() {
    // A watchdog bounds failure; it is not an application latency target.
    let watchdog = Duration::from_secs(5);
    let caller = std::thread::current().id();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let mut slow = std::pin::pin!(blocking(move || {
        entered_tx.send(std::thread::current().id()).map_err(super::err)?;
        release_rx.recv_timeout(watchdog).map_err(super::err)?;
        Ok(73)
    }));
    let mut context = Context::from_waker(Waker::noop());
    assert!(matches!(slow.as_mut().poll(&mut context), Poll::Pending),
        "a gated worker must not occupy the caller until completion");
    let worker = entered_rx.recv_timeout(watchdog).expect("worker must start");
    assert_ne!(worker, caller, "filesystem/model work ran on the command caller");
    assert!(slow.as_mut().poll(&mut context).is_pending());

    // This second command must finish while the first worker is still gated.
    assert_eq!(tauri::async_runtime::block_on(blocking(|| Ok(19))).unwrap(), 19);
    assert!(slow.as_mut().poll(&mut context).is_pending());
    release_tx.send(()).unwrap();
    assert_eq!(tauri::async_runtime::block_on(slow).unwrap(), 73);
}

#[test]
fn performance_worker_errors_leave_subsequent_commands_usable() {
    let result: CmdResult<()> = tauri::async_runtime::block_on(blocking(|| {
        Err(crate::ui_message::app("native.fixture", "synthetic worker failure"))
    }));
    assert_eq!(result.unwrap_err().code, "native.fixture");
    assert_eq!(tauri::async_runtime::block_on(blocking(|| Ok(23))).unwrap(), 23);
}

#[test]
fn performance_heavy_command_entry_points_keep_their_async_contract() {
    // Compile actual entry points, without executing IO or constructing AppState.
    // A return to a synchronous command is a compile failure here. Worker placement
    // inside each command still needs native IPC heartbeat acceptance testing.
    fn asynchronous<A, F: Future>(_: impl FnOnce(A) -> F) {}
    use super::{ai, devices, organize, settings};
    use tauri::AppHandle;

    asynchronous(|(): ()| devices::sample_levels(None, None));
    asynchronous(|(): ()| devices::list_audio_devices());
    asynchronous(devices::list_video_sources);
    asynchronous(|app: AppHandle| settings::data_info(app));
    asynchronous(|app: AppHandle| organize::list_folders(app));
    asynchronous(|app: AppHandle| organize::list_tags(app));
    asynchronous(|(app, filter): (AppHandle, bazza_store::MeetingFilter)| organize::list_meetings_filtered(app, filter));
    asynchronous(|app: AppHandle| organize::export_clip(app, String::new(), 0, 1, String::new()));
    asynchronous(|app: AppHandle| organize::create_share(app, String::new(), None, None));
    asynchronous(|app: AppHandle| organize::set_share_enabled(app, String::new(), false));
    asynchronous(|app: AppHandle| organize::delete_share(app, String::new()));
    asynchronous(|app: AppHandle| organize::export_html(app, String::new(), String::new()));
    asynchronous(|app: AppHandle| organize::change_data_dir(app, String::new()));
    asynchronous(|(): ()| organize::save_text_file(String::new(), String::new()));
    asynchronous(|app: AppHandle| ai::generate_summary(app, String::new(), None, None));
    asynchronous(|app: AppHandle| ai::send_chat(app, String::new(), String::new()));
    asynchronous(|app: AppHandle| ai::draft_followup(app, String::new()));
}

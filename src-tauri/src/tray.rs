use crate::recorder;
use crate::state::AppState;
use crate::windows;
use std::sync::atomic::Ordering;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager};

pub const TRAY_ID: &str = "bazza-tray";

#[derive(Debug, PartialEq, Eq)]
pub struct Labels {
    pub open: &'static str,
    pub toggle: &'static str,
    pub detect: &'static str,
    pub quit: &'static str,
}

pub fn labels(locale: crate::ui_locale::UiLocale, recording: bool, paused: bool) -> Labels {
    Labels {
        open: locale.text("Открыть Bazza", "Open Bazza"),
        toggle: if recording { locale.text("Остановить запись", "Stop recording") } else { locale.text("Начать запись", "Start recording") },
        detect: if paused { locale.text("Возобновить обнаружение", "Resume detection") } else { locale.text("Приостановить обнаружение", "Pause detection") },
        quit: locale.text("Выход", "Quit"),
    }
}


pub fn build(app: &AppHandle) -> tauri::Result<()> {
    let state = app.state::<AppState>();
    let text = labels(*state.ui_locale.read(), state.recording.lock().is_some(), state.detection_paused.load(Ordering::SeqCst));
    let open = MenuItem::with_id(app, "open", text.open, true, None::<&str>)?;
    let toggle = MenuItem::with_id(app, "toggle", text.toggle, true, None::<&str>)?;
    let detect = MenuItem::with_id(app, "detect", text.detect, true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", text.quit, true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &toggle, &detect, &PredefinedMenuItem::separator(app)?, &quit])?;

    TrayIconBuilder::with_id(TRAY_ID)
        .icon(app.default_window_icon().cloned().expect("app icon"))
        .tooltip("Bazza")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(move |app, ev| match ev.id().as_ref() {
            "open" => windows::focus_main(app),
            "toggle" => toggle_recording(app),
            "detect" => {
                let state = app.state::<AppState>();
                let paused = !state.detection_paused.load(Ordering::SeqCst);
                state.detection_paused.store(paused, Ordering::SeqCst);
                refresh(app);
            }
            "quit" => {
                if app.state::<AppState>().recording.lock().is_some() {
                    let _ = recorder::stop(app);
                }
                crate::bot_connection::exit_after_pending(app);
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, ev| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = ev {
                windows::focus_main(tray.app_handle());
            }
        })
        .build(app)?;
    app.manage(TrayItems { open, toggle, detect, quit });
    Ok(())
}

pub fn toggle_recording(app: &AppHandle) {
    // Off the main thread: device setup/teardown must never block the event loop.
    let app = app.clone();
    std::thread::spawn(move || {
        let is_recording = app.state::<AppState>().recording.lock().is_some();
        if is_recording {
            if let Err(e) = recorder::stop(&app) {
                recorder::notice(&app, "error", e);
            }
        } else {
            // Never start blind: the picker chooses the window and its audio first.
            recorder::open_picker(&app);
        }
        refresh(&app);
    });
}

/// Snapshot and apply every label together on the UI thread. A recording worker
/// must not retain an old locale while its individual menu updates are queued.
pub fn refresh(app: &AppHandle) {
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || refresh_on_main(&handle));
}

fn refresh_on_main(app: &AppHandle) {
    let state = app.state::<AppState>();
    let locale = *state.ui_locale.read();
    let recording = state.recording.lock().is_some();
    let detection_paused = state.detection_paused.load(Ordering::SeqCst);
    let text = labels(locale, recording, detection_paused);
    if let Some(items) = app.try_state::<TrayItems>() {
        let _ = items.open.set_text(text.open);
        let _ = items.toggle.set_text(text.toggle);
        let _ = items.detect.set_text(text.detect);
        let _ = items.quit.set_text(text.quit);
        // Opt-in native acceptance evidence, using the real menu getters. Only
        // app-owned labels and booleans are logged; no meeting/device data.
        if std::env::var_os("BAZZA_UI_LOCALE_AUDIT").is_some() {
            log::info!("ui-locale-audit {}", serde_json::json!({
                "locale": locale,
                "recording": recording,
                "detection_paused": detection_paused,
                "menu": {
                    "open": items.open.text().ok(),
                    "toggle": items.toggle.text().ok(),
                    "detect": items.detect.text().ok(),
                    "quit": items.quit.text().ok(),
                },
            }));
        }
    }
}

/// Menu items we need to update later.
pub struct TrayItems {
    pub open: MenuItem<tauri::Wry>,
    pub toggle: MenuItem<tauri::Wry>,
    pub detect: MenuItem<tauri::Wry>,
    pub quit: MenuItem<tauri::Wry>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui_locale::UiLocale;

    #[test]
    fn switching_locale_preserves_both_independent_menu_states() {
        for recording in [false, true] {
            for paused in [false, true] {
                let ru = labels(UiLocale::Ru, recording, paused);
                let en = labels(UiLocale::En, recording, paused);
                assert_eq!(ru.open, "Открыть Bazza");
                assert_eq!(en.quit, "Quit");
                assert_eq!(ru.toggle, if recording { "Остановить запись" } else { "Начать запись" });
                assert_eq!(en.toggle, if recording { "Stop recording" } else { "Start recording" });
                assert_eq!(ru.detect, if paused { "Возобновить обнаружение" } else { "Приостановить обнаружение" });
                assert_eq!(en.detect, if paused { "Resume detection" } else { "Pause detection" });
            }
        }
    }
}

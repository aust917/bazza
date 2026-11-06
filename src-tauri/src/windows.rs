//! Helpers for the three windows: `main`, `popup` (call detected), `recordbar`.

use parking_lot::Mutex;
use tauri::webview::{PageLoadEvent, PageLoadPayload};
use tauri::{AppHandle, Manager, PhysicalPosition, Webview, WebviewWindow};

const MARGIN: i32 = 16;
const OPEN_RECORDING_SETUP: &str = "window.location.hash = '/recording'";

#[derive(Default)]
pub(crate) struct RecordingSetupNavigation(Mutex<MainPage>);

#[derive(Default)]
struct MainPage {
    loaded: bool,
    recording_requested: bool,
}

pub fn refresh_titles(app: &AppHandle) {
    let locale = *app.state::<crate::state::AppState>().ui_locale.read();
    for (label, title) in [("main", "Bazza"), ("popup", "Bazza"), ("recordbar", locale.text("Bazza — запись", "Bazza recording"))] {
        if let Some(window) = window(app, label) { let _ = window.set_title(title); }
    }
}

fn window(app: &AppHandle, label: &str) -> Option<WebviewWindow> {
    app.get_webview_window(label)
}

/// Work-area rectangle of the monitor showing `hwnd` (Windows only).
#[cfg(windows)]
fn monitor_rect_for_hwnd(hwnd: isize) -> Option<(i32, i32, u32, u32)> {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONULL};
    if hwnd == 0 {
        return None;
    }
    // SAFETY: plain Win32 queries on a window handle we just enumerated; results are checked.
    unsafe {
        let hmon = MonitorFromWindow(HWND(hwnd as *mut _), MONITOR_DEFAULTTONULL);
        if hmon.is_invalid() {
            return None;
        }
        let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if !GetMonitorInfoW(hmon, &mut info).as_bool() {
            return None;
        }
        let r = info.rcWork;
        Some((r.left, r.top, (r.right - r.left).max(0) as u32, (r.bottom - r.top).max(0) as u32))
    }
}

#[cfg(not(windows))]
fn monitor_rect_for_hwnd(_hwnd: isize) -> Option<(i32, i32, u32, u32)> {
    None
}

/// Place `w` at a screen corner of the monitor the main window is on (or primary).
fn place(w: &WebviewWindow, bottom: bool, right: bool) {
    let monitor = w.current_monitor().ok().flatten().or_else(|| w.primary_monitor().ok().flatten());
    let Some(m) = monitor else { return };
    let Ok(size) = w.outer_size() else { return };
    let ms = m.size();
    let mp = m.position();
    let x = if right { mp.x + ms.width as i32 - size.width as i32 - MARGIN } else { mp.x + MARGIN };
    let y = if bottom { mp.y + ms.height as i32 - size.height as i32 - MARGIN - 48 } else { mp.y + MARGIN };
    let _ = w.set_position(PhysicalPosition::new(x, y));
}

/// Show the popup on the monitor that contains `hwnd` (Windows), else where the main window is.
pub fn show_popup_near(app: &AppHandle, hwnd: isize) {
    if let Some(w) = window(app, "popup") {
        if let Some((x, y, width, height)) = monitor_rect_for_hwnd(hwnd) {
            if let Ok(size) = w.outer_size() {
                let px = x + width as i32 - size.width as i32 - MARGIN;
                let py = y + height as i32 - size.height as i32 - MARGIN - 48;
                let _ = w.set_position(PhysicalPosition::new(px, py));
            }
        } else {
            place(&w, true, true);
        }
        let _ = w.show();
        let _ = w.set_always_on_top(true);
    }
}

pub fn hide_popup(app: &AppHandle) {
    if let Some(w) = window(app, "popup") {
        let _ = w.hide();
    }
}

pub fn show_recordbar(app: &AppHandle) {
    if let Some(w) = window(app, "recordbar") {
        place(&w, false, true);
        let _ = w.show();
        let _ = w.set_always_on_top(true);
    }
}

pub fn hide_recordbar(app: &AppHandle) {
    if let Some(w) = window(app, "recordbar") {
        let _ = w.hide();
    }
}

pub fn focus_main(app: &AppHandle) {
    if let Some(w) = window(app, "main") {
        reveal(&w);
    }
}

fn reveal(w: &WebviewWindow) {
    let _ = w.show();
    let _ = w.unminimize();
    let _ = w.set_focus();
}

pub(crate) fn open_recording_setup(app: &AppHandle) {
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        let Some(webview) = window(&handle, "main") else { return };
        reveal(&webview);
        let navigation = handle.state::<RecordingSetupNavigation>();
        let mut page = navigation.0.lock();
        if page.loaded {
            drop(page);
            let _ = webview.eval(OPEN_RECORDING_SETUP);
        } else {
            // The hotkey can arrive while the main document is still loading.
            page.recording_requested = true;
        }
    });
}

pub(crate) fn recording_setup_page_load(webview: &Webview, payload: &PageLoadPayload<'_>) {
    if webview.label() != "main" {
        return;
    }
    let navigation = webview.state::<RecordingSetupNavigation>();
    let mut page = navigation.0.lock();
    page.loaded = payload.event() == PageLoadEvent::Finished;
    let open = page.loaded && std::mem::take(&mut page.recording_requested);
    drop(page);
    if open {
        let _ = webview.eval(OPEN_RECORDING_SETUP);
    }
}

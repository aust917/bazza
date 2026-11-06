//! Attached displays, so a recording can target one monitor instead of the whole desktop.

use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MonitorInfo {
    /// Index into the list (0 = primary where the OS says so).
    pub index: u32,
    /// Identity of the capture source in the enumerated display topology.
    pub screen_id: String,
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub primary: bool,
    /// Capture-backend device id (avfoundation screen index on macOS); unused on Windows.
    pub device: u32,
}

/// Displays in a stable order, primary first. `ffmpeg` is used on macOS to
/// enumerate avfoundation screen devices; other platforms ignore it. Empty
/// means no screen could be resolved to a concrete capture source.
pub fn list_monitors(ffmpeg: Option<&Path>) -> Vec<MonitorInfo> {
    let mut v = imp::list(ffmpeg);
    v.sort_by_key(|m| (!m.primary, m.x, m.y));
    for (i, m) in v.iter_mut().enumerate() {
        m.index = i as u32;
    }
    v
}

#[cfg(windows)]
mod imp {
    use super::MonitorInfo;
    use std::path::Path;
    use windows::core::{BOOL, PCWSTR};
    use windows::Win32::Foundation::{LPARAM, RECT};
    use windows::Win32::Graphics::Gdi::{EnumDisplayDevicesW, EnumDisplayMonitors, GetMonitorInfoW, DISPLAY_DEVICEW, HDC, HMONITOR, MONITORINFOEXW};

    const MONITORINFOF_PRIMARY: u32 = 1;
    // Request the monitor interface path in DISPLAY_DEVICE.DeviceID.
    const GET_DEVICE_INTERFACE_NAME: u32 = 1;

    pub fn list(_ffmpeg: Option<&Path>) -> Vec<MonitorInfo> {
        let mut out: Vec<MonitorInfo> = Vec::new();
        // SAFETY: the callback only uses `out` through LPARAM for the duration of the synchronous enumeration.
        unsafe {
            let _ = EnumDisplayMonitors(None, None, Some(cb), LPARAM(&mut out as *mut Vec<MonitorInfo> as isize));
        }
        out
    }

    unsafe extern "system" fn cb(hmon: HMONITOR, _hdc: HDC, _rect: *mut RECT, lparam: LPARAM) -> BOOL {
        let out = &mut *(lparam.0 as *mut Vec<MonitorInfo>);
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        if GetMonitorInfoW(hmon, &mut info.monitorInfo).as_bool() {
            let r = info.monitorInfo.rcMonitor;
            let raw = String::from_utf16_lossy(&info.szDevice);
            let name = raw.trim_end_matches('\0').trim_start_matches(r"\\.\").to_string();
            let width = (r.right - r.left).max(0) as u32;
            let height = (r.bottom - r.top).max(0) as u32;
            if width == 0 || height == 0 {
                return BOOL(1);
            }
            let mut display = DISPLAY_DEVICEW { cb: std::mem::size_of::<DISPLAY_DEVICEW>() as u32, ..Default::default() };
            let interface = if EnumDisplayDevicesW(PCWSTR(info.szDevice.as_ptr()), 0, &mut display, GET_DEVICE_INTERFACE_NAME).as_bool() {
                String::from_utf16_lossy(&display.DeviceID).trim_end_matches('\0').to_owned()
            } else {
                String::new()
            };
            out.push(MonitorInfo {
                index: out.len() as u32,
                screen_id: format!("gdi:{name}:{interface}:{}:{}:{width}:{height}", r.left, r.top),
                name: if name.is_empty() { format!("Display {}", out.len() + 1) } else { name },
                x: r.left,
                y: r.top,
                width,
                height,
                primary: info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
                device: 0,
            });
        }
        BOOL(1)
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use super::MonitorInfo;
    use std::path::Path;

    /// `ffmpeg -f avfoundation -list_devices true -i ""` prints lines like
    /// `[AVFoundation indev @ 0x...] [2] Capture screen 0`; the number in brackets
    /// is the device index avfoundation wants.
    pub fn list(ffmpeg: Option<&Path>) -> Vec<MonitorInfo> {
        let Some(ffmpeg) = ffmpeg else { return Vec::new() };
        let out = std::process::Command::new(ffmpeg).args(["-hide_banner", "-f", "avfoundation", "-list_devices", "true", "-i", ""]).output();
        let Ok(out) = out else { return Vec::new() };
        let text = String::from_utf8_lossy(&out.stderr);
        parse_avfoundation(&text)
    }

    pub fn parse_avfoundation(text: &str) -> Vec<MonitorInfo> {
        let mut v = Vec::new();
        for line in text.lines() {
            let Some(pos) = line.find("Capture screen") else { continue };
            let head = &line[..pos];
            let Some(open) = head.rfind('[') else { continue };
            let Some(close) = head[open..].find(']') else { continue };
            let Ok(index) = head[open + 1..open + close].trim().parse::<u32>() else { continue };
            let name = line[pos..].trim().to_string();
            v.push(MonitorInfo { index, screen_id: format!("avfoundation:{index}:{name}"), name, x: 0, y: 0, width: 0, height: 0, primary: v.is_empty(), device: index });
        }
        v
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
mod imp {
    use super::MonitorInfo;
    use std::path::Path;
    pub fn list(_ffmpeg: Option<&Path>) -> Vec<MonitorInfo> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enumerated_displays_are_indexed_and_primary_first() {
        let m = list_monitors(None);
        assert!(m.iter().enumerate().all(|(index, monitor)| monitor.index as usize == index && !monitor.screen_id.is_empty()));
        assert!(m.iter().filter(|x| x.primary).count() <= 1 || cfg!(not(windows)));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn parses_avfoundation_listing() {
        let text = "[AVFoundation indev @ 0x1] AVFoundation video devices:\n[AVFoundation indev @ 0x1] [0] FaceTime HD Camera\n[AVFoundation indev @ 0x1] [1] Capture screen 0\n[AVFoundation indev @ 0x1] [2] Capture screen 1\n";
        let v = imp::parse_avfoundation(text);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].device, 1);
        assert_eq!(v[1].device, 2);
    }
}

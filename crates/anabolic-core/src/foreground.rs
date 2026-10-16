//! The program in front: whose window has the foreground, and whether that
//! window covers its whole monitor (a game, full screen or borderless).
//! Window manager queries only; the program is not asked anything, so this
//! is safe beside anti-cheat.

use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetClassNameW, GetForegroundWindow, GetWindowRect, GetWindowThreadProcessId,
};

use crate::util::from_wide;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Front {
    pub pid: u32,
    /// Its window covers its whole monitor.
    pub fullscreen: bool,
}

/// The desktop and the taskbar cover the screen too; they are not games.
fn shell(class: &str) -> bool {
    matches!(
        class,
        "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd"
    )
}

pub fn front() -> Option<Front> {
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.0.is_null() {
            return None;
        }
        let mut class = [0u16; 64];
        let n = GetClassNameW(hwnd, &mut class) as usize;
        if shell(&from_wide(&class[..n])) {
            return None;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == 0 {
            return None;
        }
        let mut window = RECT::default();
        GetWindowRect(hwnd, &mut window).ok()?;
        let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        let fullscreen = GetMonitorInfoW(monitor, &mut info).as_bool()
            && window.left <= info.rcMonitor.left
            && window.top <= info.rcMonitor.top
            && window.right >= info.rcMonitor.right
            && window.bottom >= info.rcMonitor.bottom;
        Some(Front { pid, fullscreen })
    }
}

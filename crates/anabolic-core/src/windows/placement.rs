// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Keeping Anabolic's window where it is of use: off the monitor a
//! full-screen game fills (called up during a game, it should not cover
//! it), and on a monitor that exists (one unplugged or switched off would
//! otherwise keep it out of sight).

use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITOR_DEFAULTTONEAREST,
    MONITOR_DEFAULTTONULL, MONITORINFO, MonitorFromRect, MonitorFromWindow,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetWindowRect, IsZoomed, MONITORINFOF_PRIMARY, SW_MAXIMIZE, SW_RESTORE,
    SWP_NOACTIVATE, SWP_NOZORDER, SetWindowPos, ShowWindow,
};
use windows::core::BOOL;

struct Monitor {
    handle: HMONITOR,
    work: RECT,
    primary: bool,
}

fn monitors() -> Vec<Monitor> {
    unsafe extern "system" fn visit(
        m: HMONITOR,
        _: HDC,
        _: *mut RECT,
        list: windows::Win32::Foundation::LPARAM,
    ) -> BOOL {
        let list = unsafe { &mut *(list.0 as *mut Vec<Monitor>) };
        let mut info = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if unsafe { GetMonitorInfoW(m, &mut info) }.as_bool() {
            list.push(Monitor {
                handle: m,
                work: info.rcWork,
                primary: info.dwFlags & MONITORINFOF_PRIMARY != 0,
            });
        }
        BOOL(1)
    }
    let mut list: Vec<Monitor> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(visit),
            windows::Win32::Foundation::LPARAM(&raw mut list as isize),
        );
    }
    list
}

fn area(r: &RECT) -> i64 {
    (r.right - r.left) as i64 * (r.bottom - r.top) as i64
}

/// The monitor a full-screen window in front covers (a game, full screen
/// or borderless); None when nothing like that is in front.
pub fn fullscreen_monitor() -> Option<isize> {
    let front = crate::foreground::front()?;
    if !front.fullscreen || front.pid == std::process::id() {
        return None;
    }
    let hwnd = unsafe { GetForegroundWindow() };
    Some(unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) }.0 as isize)
}

/// Moves `hwnd`, its size kept where it fits, to the middle of `to`'s
/// work area; maximized stays maximized.
fn move_to(hwnd: HWND, to: &Monitor) {
    unsafe {
        let zoomed = IsZoomed(hwnd).as_bool();
        if zoomed {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        let mut r = RECT::default();
        if GetWindowRect(hwnd, &mut r).is_err() {
            return;
        }
        let w = (r.right - r.left).min(to.work.right - to.work.left);
        let h = (r.bottom - r.top).min(to.work.bottom - to.work.top);
        let x = to.work.left + (to.work.right - to.work.left - w) / 2;
        let y = to.work.top + (to.work.bottom - to.work.top - h) / 2;
        let _ = SetWindowPos(hwnd, None, x, y, w, h, SWP_NOZORDER | SWP_NOACTIVATE);
        if zoomed {
            let _ = ShowWindow(hwnd, SW_MAXIMIZE);
        }
    }
}

/// When `hwnd` is on the monitor `avoid` (a game fills it), moves it to the
/// biggest other one. Returns whether it moved; with one monitor there is
/// nowhere to go.
pub fn avoid(hwnd: HWND, avoid: isize) -> bool {
    let on = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    if on.0 as isize != avoid {
        return false;
    }
    let all = monitors();
    let Some(best) = all
        .iter()
        .filter(|m| m.handle.0 as isize != avoid)
        .max_by_key(|m| (area(&m.work), m.primary))
    else {
        return false;
    };
    move_to(hwnd, best);
    true
}

/// When the title bar of `hwnd` is on no monitor at all (its monitor was
/// unplugged, switched off or rearranged), brings it to the primary one.
/// Returns whether it moved.
pub fn rescue(hwnd: HWND) -> bool {
    let mut r = RECT::default();
    if unsafe { GetWindowRect(hwnd, &mut r) }.is_err() {
        return false;
    }
    // The strip one grabs to move a window: enough of it must be visible.
    let strip = RECT {
        left: r.left + 40,
        top: r.top,
        right: (r.right - 40).max(r.left + 41),
        bottom: r.top + 24,
    };
    if !unsafe { MonitorFromRect(&strip, MONITOR_DEFAULTTONULL) }.is_invalid() {
        return false;
    }
    let all = monitors();
    let Some(primary) = all.iter().find(|m| m.primary).or(all.first()) else {
        return false;
    };
    move_to(hwnd, primary);
    true
}

/// Whether a point is on any monitor (for tests and callers that only
/// have a position).
pub fn visible(x: i32, y: i32) -> bool {
    use windows::Win32::Graphics::Gdi::MonitorFromPoint;
    !unsafe { MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONULL) }.is_invalid()
}

#[cfg(test)]
mod tests {
    #[test]
    fn finds_the_monitors() {
        let all = super::monitors();
        assert!(!all.is_empty());
        assert_eq!(all.iter().filter(|m| m.primary).count(), 1);
        assert!(super::visible(10, 10));
        assert!(!super::visible(-100_000, -100_000));
    }
}

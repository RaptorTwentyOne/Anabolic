//! One Anabolic per session. The first instance creates a named event and
//! waits on it; a later one (a second double-click, Ctrl+Shift+Esc pressed
//! again while the first is still starting) signals it and exits, and the
//! first brings its window to the front. The Ctrl+Shift+Esc launcher signals
//! the same event itself, so a running Anabolic comes up at once without
//! anything being started.
//!
//! The event is created before anything else happens at startup, so two
//! instances starting together cannot both miss each other the way a search
//! for an existing window could.

use std::thread;
use std::time::Duration;

use windows::Win32::Foundation::{
    ERROR_ALREADY_EXISTS, GetLastError, HANDLE, HLOCAL, HWND, LPARAM, LocalFree, WAIT_OBJECT_0,
};
use windows::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
use windows::Win32::System::Threading::{
    CreateEventW, INFINITE, OpenProcess, PROCESS_SYNCHRONIZE, SetEvent, WaitForSingleObject,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GW_OWNER, GetForegroundWindow, GetWindow, GetWindowTextW,
    GetWindowThreadProcessId, IsIconic, IsWindowVisible, SW_RESTORE, SW_SHOW, SetForegroundWindow,
    ShowWindow, SwitchToThisWindow,
};
use windows::core::{BOOL, PCWSTR, w};

use crate::util::{OwnedHandle, wide};

/// Must match the launcher's copy (crates/anabolic-launch).
pub const ACTIVATE_EVENT: &str = r"Local\Anabolic-Activate";

/// Anyone signed in may signal it (the launcher runs unelevated while
/// Anabolic usually runs elevated), and the low integrity label lets a
/// medium integrity process write to an object a high one created.
const SECURITY: PCWSTR = w!("D:(A;;0x1F0003;;;AU)(A;;0x1F0003;;;SY)S:(ML;;NW;;;LW)");

pub enum Claim {
    /// No other instance: serve activations with this event.
    First(OwnedHandle),
    /// Another instance runs and has been asked to come to the front.
    Running,
    /// The event could not be created; run without single-instance.
    Unavailable,
}

/// Becomes the session's Anabolic, or hands over to the one already running.
pub fn claim() -> Claim {
    claim_named(ACTIVATE_EVENT)
}

fn claim_named(name: &str) -> Claim {
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    let have_descriptor = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            SECURITY,
            SDDL_REVISION_1,
            &mut descriptor,
            None,
        )
    }
    .is_ok();
    let attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: false.into(),
    };
    let name = wide(name);
    let created = unsafe {
        CreateEventW(
            have_descriptor.then_some(&attributes as *const _),
            false,
            false,
            PCWSTR(name.as_ptr()),
        )
    };
    let existed = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
    if have_descriptor {
        unsafe {
            let _ = LocalFree(Some(HLOCAL(descriptor.0)));
        }
    }
    match created {
        Ok(handle) if existed => {
            unsafe {
                let _ = SetEvent(handle);
            }
            drop(OwnedHandle(handle));
            Claim::Running
        }
        Ok(handle) => Claim::First(OwnedHandle(handle)),
        Err(_) => Claim::Unavailable,
    }
}

/// Waits up to `timeout` for process `pid` to exit: the instance that
/// started us elevated and is closing to make room.
pub fn await_exit(pid: u32, timeout: Duration) {
    if let Ok(process) = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid) } {
        let process = OwnedHandle(process);
        unsafe {
            WaitForSingleObject(process.raw(), timeout.as_millis() as u32);
        }
    }
}

/// Runs `on_activate` on a thread of its own every time another instance or
/// the launcher asks for this one.
pub fn serve(event: OwnedHandle, on_activate: impl Fn() + Send + 'static) {
    // Stays open as long as the process: it is what makes this instance the
    // one the others find.
    let raw = event.raw().0 as usize;
    std::mem::forget(event);
    let spawned = thread::Builder::new()
        .name("anabolic-activate".into())
        .spawn(move || {
            let handle = HANDLE(raw as *mut _);
            while unsafe { WaitForSingleObject(handle, INFINITE) } == WAIT_OBJECT_0 {
                on_activate();
            }
        });
    if let Err(e) = spawned {
        eprintln!("anabolic: no activation thread: {e}");
    }
}

/// Restores and raises this process' main window. Waits a few seconds for
/// it when the request came while the window was still being created.
pub fn bring_to_front() -> bool {
    for _ in 0..50 {
        if let Some(hwnd) = own_window() {
            // Ctrl+Shift+Esc during a full-screen game: open beside it, on
            // another monitor, rather than over it. Before anything else,
            // while the game is still the foreground window.
            if let Some(game) = crate::placement::fullscreen_monitor() {
                crate::placement::avoid(hwnd, game);
            }
            crate::placement::rescue(hwnd);
            unsafe {
                if IsIconic(hwnd).as_bool() {
                    let _ = ShowWindow(hwnd, SW_RESTORE);
                } else if !IsWindowVisible(hwnd).as_bool() {
                    let _ = ShowWindow(hwnd, SW_SHOW);
                }
                // The launcher passed on its right to take the foreground;
                // if that did not reach us, do what Alt+Tab does.
                if !SetForegroundWindow(hwnd).as_bool() || GetForegroundWindow() != hwnd {
                    SwitchToThisWindow(hwnd, true);
                }
            }
            return true;
        }
        thread::sleep(Duration::from_millis(100));
    }
    false
}

/// Paints the title bar dark, in `caption` and `border` (0xRRGGBB), so it
/// belongs to the window instead of sitting on it in Windows' light gray.
/// The colors need Windows 11; the dark mode works on Windows 10 too.
/// Returns false while the window does not exist yet.
pub fn style_title_bar(caption: u32, border: u32) -> bool {
    use windows::Win32::Graphics::Dwm::{
        DWMWA_BORDER_COLOR, DWMWA_CAPTION_COLOR, DWMWA_USE_IMMERSIVE_DARK_MODE,
        DwmSetWindowAttribute,
    };
    let Some(hwnd) = own_window() else {
        return false;
    };
    // COLORREF is 0x00BBGGRR.
    let colorref = |rgb: u32| ((rgb & 0xFF) << 16) | (rgb & 0xFF00) | ((rgb >> 16) & 0xFF);
    let dark: u32 = 1;
    let caption = colorref(caption);
    let border = colorref(border);
    unsafe {
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            (&raw const dark).cast(),
            4,
        );
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_CAPTION_COLOR, (&raw const caption).cast(), 4);
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_BORDER_COLOR, (&raw const border).cast(), 4);
    }
    true
}

/// This process' top-level "Anabolic…" window.
pub fn own_window() -> Option<HWND> {
    struct Search {
        pid: u32,
        found: Option<HWND>,
    }
    unsafe extern "system" fn visit(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let search = unsafe { &mut *(lparam.0 as *mut Search) };
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
        if pid != search.pid || unsafe { GetWindow(hwnd, GW_OWNER) }.is_ok_and(|o| !o.is_invalid())
        {
            return BOOL(1);
        }
        let mut title = [0u16; 64];
        let len = unsafe { GetWindowTextW(hwnd, &mut title) } as usize;
        if !String::from_utf16_lossy(&title[..len]).starts_with("Anabolic") {
            return BOOL(1);
        }
        search.found = Some(hwnd);
        BOOL(0)
    }
    let mut search = Search {
        pid: std::process::id(),
        found: None,
    };
    let _ = unsafe { EnumWindows(Some(visit), LPARAM(&raw mut search as isize)) };
    search.found
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn second_claim_signals_the_first() {
        let name = format!(r"Local\Anabolic-Activate-test-{}", std::process::id());
        let Claim::First(event) = claim_named(&name) else {
            panic!("the first claim should own the event");
        };
        let (tx, rx) = mpsc::channel();
        serve(event, move || {
            let _ = tx.send(());
        });
        assert!(matches!(claim_named(&name), Claim::Running));
        rx.recv_timeout(Duration::from_secs(2))
            .expect("the first instance was not signalled");
        // And again: the event resets itself.
        assert!(matches!(claim_named(&name), Claim::Running));
        rx.recv_timeout(Duration::from_secs(2))
            .expect("the second signal was lost");
    }
}

//! Registered as taskmgr.exe's "debugger", so Windows runs this instead of
//! Task Manager (Ctrl+Shift+Esc, the taskbar menu, `taskmgr`). It runs
//! unelevated and must be quick:
//!
//! 1. Anabolic already running: signal its activation event; it comes to the
//!    front. Nothing is started, so this is instant.
//! 2. Otherwise start it through its elevated scheduled task (no UAC prompt).
//!    Pressed twice while that starts, the second Anabolic finds the first
//!    one's event and hands over to it.
//! 3. No task: start it directly.
//!
//! Command line: `anabolic-launch.exe "<path to anabolic.exe>" <whatever
//! Windows appends: taskmgr.exe and its arguments>`.

#![windows_subsystem = "windows"]

use std::os::windows::process::CommandExt;
use std::process::{Command, Stdio};

use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Threading::{EVENT_MODIFY_STATE, OpenEventW, SetEvent};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{ASFW_ANY, AllowSetForegroundWindow, SW_SHOWNORMAL};
use windows::core::{PCWSTR, w};

/// Must match `anabolic_core::taskmgr::TASK_NAME`.
const TASK_NAME: &str = "Anabolic Elevated Start";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn main() {
    // Started by the user's keypress, we may hand the foreground on to
    // Anabolic, which is not.
    unsafe {
        let _ = AllowSetForegroundWindow(ASFW_ANY);
    }
    if signal_running() {
        return;
    }
    let started = Command::new("schtasks")
        .args(["/run", "/tn", TASK_NAME])
        .creation_flags(CREATE_NO_WINDOW)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if started {
        return;
    }
    let Some(app) = std::env::args()
        .nth(1)
        .filter(|a| a.to_lowercase().ends_with("anabolic.exe"))
    else {
        return;
    };
    let app: Vec<u16> = app.encode_utf16().chain([0]).collect();
    unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            PCWSTR(app.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
    }
}

/// Asks a running Anabolic to come to the front. Must match
/// `anabolic_core::instance::ACTIVATE_EVENT`.
fn signal_running() -> bool {
    let Ok(event) =
        (unsafe { OpenEventW(EVENT_MODIFY_STATE, false, w!(r"Local\Anabolic-Activate")) })
    else {
        return false;
    };
    let signalled = unsafe { SetEvent(event) }.is_ok();
    unsafe {
        let _ = CloseHandle(event);
    }
    signalled
}

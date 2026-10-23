//! The end of the session. Signing out or shutting down, Windows tells each
//! top-level window (WM_ENDSESSION) and ends the process once they have
//! answered: winit's windows answer at once, and the settings changed since
//! the last autosave were lost. A hidden window of our own holds the answer
//! back until Anabolic has quit as from the notification area's Quit:
//! settings saved, kernel traces stopped.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use eframe::egui;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, MSG, RegisterClassW,
    TranslateMessage, WINDOW_EX_STYLE, WM_ENDSESSION, WM_QUERYENDSESSION, WNDCLASSW, WS_OVERLAPPED,
};
use windows::core::w;

/// Windows asked Anabolic to end.
static ENDING: AtomicBool = AtomicBool::new(false);
/// Anabolic has saved and stopped what it runs.
static DONE: AtomicBool = AtomicBool::new(false);
/// Who to wake when it is asked.
static CTX: std::sync::OnceLock<egui::Context> = std::sync::OnceLock::new();

/// Whether Windows asked Anabolic to end: then it quits.
pub fn ending() -> bool {
    ENDING.load(Ordering::Relaxed)
}

/// Anabolic is done; Windows may end it now.
pub fn done() {
    DONE.store(true, Ordering::Relaxed);
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        // Anabolic never stands in the way.
        WM_QUERYENDSESSION => LRESULT(1),
        WM_ENDSESSION if wparam.0 != 0 => {
            ENDING.store(true, Ordering::Relaxed);
            if let Some(ctx) = CTX.get() {
                ctx.request_repaint();
            }
            // Once this returns Windows may end the process at any moment:
            // give the quit a few seconds to save first.
            let started = Instant::now();
            while !DONE.load(Ordering::Relaxed) && started.elapsed() < Duration::from_secs(5) {
                std::thread::sleep(Duration::from_millis(20));
            }
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// Starts listening, on a thread of its own: the UI thread may be busy or
/// asleep when Windows asks.
pub fn watch(ctx: &egui::Context) {
    if CTX.set(ctx.clone()).is_err() {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("anabolic-session".into())
        .spawn(|| unsafe {
            let Ok(module) = GetModuleHandleW(None) else {
                return;
            };
            let instance = module.into();
            let class = w!("AnabolicSessionEnd");
            let wc = WNDCLASSW {
                lpfnWndProc: Some(window_proc),
                hInstance: instance,
                lpszClassName: class,
                ..Default::default()
            };
            RegisterClassW(&wc);
            // Top-level (only those are asked), never shown, and without a
            // title: the search for Anabolic's own window looks for one
            // starting with "Anabolic".
            if CreateWindowExW(
                WINDOW_EX_STYLE(0),
                class,
                w!(""),
                WS_OVERLAPPED,
                0,
                0,
                0,
                0,
                None,
                None,
                Some(instance),
                None,
            )
            .is_err()
            {
                return;
            }
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        });
    if let Err(e) = spawned {
        eprintln!("anabolic: no session thread: {e}");
    }
}

//! The notification area icon. With its window closed Anabolic keeps
//! running (the history is recorded, rules applied); the icon brings the
//! window back or quits. A thread of its own owns a hidden window that
//! receives the icon's clicks, so they work while the UI thread sleeps.

use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use eframe::egui;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIIF_INFO, NIIF_RESPECT_QUIET_TIME,
    NIIF_WARNING, NIM_ADD, NIM_DELETE, NIM_MODIFY, NIM_SETVERSION, NOTIFYICON_VERSION_4,
    NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DispatchMessageW,
    GWLP_USERDATA, GetCursorPos, GetMessageW, GetSystemMetrics, GetWindowLongPtrW, HICON,
    IMAGE_ICON, LR_DEFAULTCOLOR, LoadImageW, MF_SEPARATOR, MF_STRING, MSG, PostMessageW,
    PostQuitMessage, RegisterClassW, RegisterWindowMessageW, SM_CXSMICON, SM_CYSMICON,
    SetForegroundWindow, SetMenuDefaultItem, SetWindowLongPtrW, TPM_NONOTIFY, TPM_RETURNCMD,
    TPM_RIGHTBUTTON, TrackPopupMenu, TranslateMessage, WINDOW_EX_STYLE, WM_APP, WM_CLOSE,
    WM_CONTEXTMENU, WM_DESTROY, WM_LBUTTONUP, WM_NULL, WNDCLASSW, WS_OVERLAPPED,
};
use windows::core::{PCWSTR, w};

pub enum TrayEvent {
    /// The window was brought back.
    Open,
    Quit,
}

pub struct Tray {
    hwnd: isize,
    events: Receiver<TrayEvent>,
}

const WM_TRAY: u32 = WM_APP + 1;
const ICON_ID: u32 = 1;
const OPEN: usize = 1;
const QUIT: usize = 2;
/// Enter on the focused icon (NIN_SELECT | NINF_KEY).
const NIN_KEYSELECT: u32 = 0x401;
/// A click on one of the icon's notifications.
const NIN_BALLOONUSERCLICK: u32 = 0x405;

struct State {
    events: Sender<TrayEvent>,
    ctx: egui::Context,
    icon: HICON,
    /// Sent to every top-level window when Explorer (re)starts: the icon
    /// has to be added again.
    taskbar_created: u32,
}

/// Copies `text` into a fixed, nul-terminated buffer, cut to fit.
fn fill(dst: &mut [u16], text: &str) {
    let n = dst.len() - 1;
    let mut i = 0;
    for c in text.encode_utf16().take(n) {
        dst[i] = c;
        i += 1;
    }
    dst[i] = 0;
}

fn data(hwnd: HWND) -> NOTIFYICONDATAW {
    NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: ICON_ID,
        ..Default::default()
    }
}

fn add_icon(hwnd: HWND, icon: HICON) -> bool {
    let mut d = data(hwnd);
    d.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP;
    d.uCallbackMessage = WM_TRAY;
    d.hIcon = icon;
    fill(&mut d.szTip, "Anabolic");
    unsafe {
        if !Shell_NotifyIconW(NIM_ADD, &d).as_bool() {
            return false;
        }
        d.Anonymous.uVersion = NOTIFYICON_VERSION_4;
        let _ = Shell_NotifyIconW(NIM_SETVERSION, &d);
    }
    true
}

fn open(state: &State) {
    crate::come_forward(&state.ctx);
    let _ = state.events.send(TrayEvent::Open);
}

fn menu(hwnd: HWND, state: &State) {
    unsafe {
        let Ok(menu) = CreatePopupMenu() else {
            return;
        };
        let _ = AppendMenuW(menu, MF_STRING, OPEN, w!("Open Anabolic"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING, QUIT, w!("Quit Anabolic"));
        let _ = SetMenuDefaultItem(menu, OPEN as u32, 0);
        let mut at = POINT::default();
        let _ = GetCursorPos(&mut at);
        // Without this the menu does not close when clicked away from.
        let _ = SetForegroundWindow(hwnd);
        let chosen = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY,
            at.x,
            at.y,
            None,
            hwnd,
            None,
        );
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
        match chosen.0 as usize {
            OPEN => open(state),
            QUIT => {
                let _ = state.events.send(TrayEvent::Quit);
                state.ctx.request_repaint();
            }
            _ => {}
        }
    }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let state = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const State;
    if state.is_null() {
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    }
    let state = unsafe { &*state };
    match msg {
        WM_TRAY => {
            // Version 4: the event is in the low word of lParam.
            match (lparam.0 & 0xFFFF) as u32 {
                WM_LBUTTONUP | NIN_KEYSELECT | NIN_BALLOONUSERCLICK => open(state),
                WM_CONTEXTMENU => menu(hwnd, state),
                _ => {}
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            unsafe {
                let _ = Shell_NotifyIconW(NIM_DELETE, &data(hwnd));
                PostQuitMessage(0);
            }
            LRESULT(0)
        }
        m if m == state.taskbar_created => {
            add_icon(hwnd, state.icon);
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

impl Tray {
    /// Puts the icon in the notification area. None when there is no
    /// notification area to put it in (no Explorer on this desktop): then
    /// closing the window must not hide it.
    pub fn start(ctx: egui::Context) -> Option<Tray> {
        let (events, rx) = mpsc::channel();
        let (ready, ready_rx) = mpsc::channel::<Option<isize>>();
        let spawned = std::thread::Builder::new()
            .name("anabolic-tray".into())
            .spawn(move || unsafe {
                let Ok(module) = GetModuleHandleW(None) else {
                    let _ = ready.send(None);
                    return;
                };
                let instance = module.into();
                let class = w!("AnabolicNotifyIcon");
                let wc = WNDCLASSW {
                    lpfnWndProc: Some(window_proc),
                    hInstance: instance,
                    lpszClassName: class,
                    ..Default::default()
                };
                // Fails harmlessly when a previous icon registered it.
                RegisterClassW(&wc);
                // No title: the window search for Anabolic's own window
                // looks for one starting with "Anabolic".
                let Ok(hwnd) = CreateWindowExW(
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
                ) else {
                    let _ = ready.send(None);
                    return;
                };
                // The icon embedded in the executable (resource 1), small size.
                let icon = LoadImageW(
                    Some(instance),
                    // MAKEINTRESOURCE(1): an id, not an address.
                    PCWSTR(std::ptr::without_provenance(1)),
                    IMAGE_ICON,
                    GetSystemMetrics(SM_CXSMICON),
                    GetSystemMetrics(SM_CYSMICON),
                    LR_DEFAULTCOLOR,
                )
                .map(|h| HICON(h.0))
                .unwrap_or_default();
                let state = Box::into_raw(Box::new(State {
                    events,
                    ctx,
                    icon,
                    taskbar_created: RegisterWindowMessageW(w!("TaskbarCreated")),
                }));
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, state as isize);
                if !add_icon(hwnd, icon) {
                    SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                    let _ = windows::Win32::UI::WindowsAndMessaging::DestroyWindow(hwnd);
                    drop(Box::from_raw(state));
                    let _ = ready.send(None);
                    return;
                }
                let _ = ready.send(Some(hwnd.0 as isize));
                let mut msg = MSG::default();
                while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                drop(Box::from_raw(state));
            });
        if spawned.is_err() {
            return None;
        }
        let hwnd = ready_rx.recv_timeout(Duration::from_secs(3)).ok()??;
        Some(Tray { hwnd, events: rx })
    }

    fn hwnd(&self) -> HWND {
        HWND(self.hwnd as *mut _)
    }

    pub fn events(&self) -> impl Iterator<Item = TrayEvent> + '_ {
        self.events.try_iter()
    }

    /// The text shown when the pointer rests on the icon.
    pub fn tip(&self, text: &str) {
        let mut d = data(self.hwnd());
        d.uFlags = NIF_TIP | NIF_SHOWTIP;
        fill(&mut d.szTip, text);
        unsafe {
            let _ = Shell_NotifyIconW(NIM_MODIFY, &d);
        }
    }

    /// A notification from the icon; quiet while Windows' focus assist is.
    pub fn notify(&self, title: &str, text: &str, warning: bool) {
        let mut d = data(self.hwnd());
        d.uFlags = NIF_INFO;
        fill(&mut d.szInfoTitle, title);
        fill(&mut d.szInfo, text);
        d.dwInfoFlags = if warning { NIIF_WARNING } else { NIIF_INFO } | NIIF_RESPECT_QUIET_TIME;
        unsafe {
            let _ = Shell_NotifyIconW(NIM_MODIFY, &d);
        }
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        // The thread removes the icon and ends with its window.
        unsafe {
            let _ = PostMessageW(Some(self.hwnd()), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }
}

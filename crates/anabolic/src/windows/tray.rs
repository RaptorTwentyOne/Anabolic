// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! The notification area icon. With its window closed Anabolic keeps
//! running (the history is recorded, rules applied); the icon brings the
//! window back or quits. A thread of its own owns a hidden window that
//! receives the icon's clicks, so they work while the UI thread sleeps.

use std::cell::RefCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use eframe::egui;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateBitmap, CreateCompatibleDC, CreateDIBSection,
    DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDIBits,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIIF_INFO, NIIF_RESPECT_QUIET_TIME,
    NIIF_WARNING, NIM_ADD, NIM_DELETE, NIM_MODIFY, NIM_SETVERSION, NOTIFYICON_VERSION_4,
    NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreateIconIndirect, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyIcon,
    DestroyMenu, DispatchMessageW, GWLP_USERDATA, GetCursorPos, GetIconInfo, GetMessageW,
    GetSystemMetrics, GetWindowLongPtrW, HICON, ICONINFO, IMAGE_ICON, LR_DEFAULTCOLOR, LoadImageW,
    MF_SEPARATOR, MF_STRING, MSG, PostMessageW, PostQuitMessage, RegisterClassW,
    RegisterWindowMessageW, SM_CXSMICON, SM_CYSMICON, SetForegroundWindow, SetMenuDefaultItem,
    SetWindowLongPtrW, TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu,
    TranslateMessage, WINDOW_EX_STYLE, WM_APP, WM_CLOSE, WM_CONTEXTMENU, WM_DESTROY, WM_LBUTTONUP,
    WM_NULL, WNDCLASSW, WS_OVERLAPPED,
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
    meter: RefCell<Meter>,
    readded: Arc<AtomicBool>,
}

/// The CPU meter drawn into the icon (see `meter`).
struct Meter {
    /// The logo's pixels at the icon's size, RGBA; empty when unreadable.
    logo: Vec<u8>,
    size: usize,
    /// The level and colour shown; None while the plain logo is.
    shown: Option<(usize, bool)>,
    /// The icon drawn last, destroyed once replaced.
    icon: HICON,
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
    /// The icon was added again with the plain logo: the meter has to be
    /// drawn again.
    readded: Arc<AtomicBool>,
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

/// A 32-bit DIB section header, top-down.
fn dib_header(size: i32) -> BITMAPINFO {
    BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: size,
            biHeight: -size,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    }
}

/// The executable's icon at `size` pixels, RGBA.
fn logo_pixels(size: i32) -> Option<Vec<u8>> {
    unsafe {
        let module = GetModuleHandleW(None).ok()?;
        let icon = LoadImageW(
            Some(module.into()),
            PCWSTR(std::ptr::without_provenance(1)),
            IMAGE_ICON,
            size,
            size,
            LR_DEFAULTCOLOR,
        )
        .ok()?;
        let icon = HICON(icon.0);
        let pixels = icon_pixels(icon, size);
        let _ = DestroyIcon(icon);
        pixels
    }
}

/// An icon's pixels, RGBA, `size` square.
fn icon_pixels(icon: HICON, size: i32) -> Option<Vec<u8>> {
    unsafe {
        let mut info = ICONINFO::default();
        GetIconInfo(icon, &mut info).ok()?;
        let dc = CreateCompatibleDC(None);
        let mut header = dib_header(size);
        let mut pixels = vec![0u8; (size * size * 4) as usize];
        let lines = GetDIBits(
            dc,
            info.hbmColor,
            0,
            size as u32,
            Some(pixels.as_mut_ptr().cast()),
            &mut header,
            DIB_RGB_COLORS,
        );
        let _ = DeleteDC(dc);
        let _ = DeleteObject(info.hbmColor.into());
        let _ = DeleteObject(info.hbmMask.into());
        if lines == 0 {
            return None;
        }
        for px in pixels.as_chunks_mut::<4>().0 {
            px.swap(0, 2);
        }
        // An icon drawn without alpha would come out invisible.
        if pixels.as_chunks::<4>().0.iter().all(|p| p[3] == 0) {
            for px in pixels.as_chunks_mut::<4>().0 {
                px[3] = 255;
            }
        }
        Some(pixels)
    }
}

/// An icon from RGBA pixels (straight alpha), `size` square.
fn icon_from(rgba: &[u8], size: i32) -> Option<HICON> {
    unsafe {
        let header = dib_header(size);
        let mut bits = std::ptr::null_mut();
        let color = CreateDIBSection(None, &header, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
        let dst = std::slice::from_raw_parts_mut(bits.cast::<u8>(), rgba.len());
        for (d, s) in dst
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip(rgba.as_chunks::<4>().0)
        {
            *d = [s[2], s[1], s[0], s[3]];
        }
        // The alpha says what shows; the mask has to be there all the same.
        let row = (size as usize).div_ceil(16) * 2;
        let zeros = vec![0u8; row * size as usize];
        let mask = CreateBitmap(size, size, 1, 1, Some(zeros.as_ptr().cast()));
        let info = ICONINFO {
            fIcon: true.into(),
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: mask,
            hbmColor: color,
        };
        let icon = CreateIconIndirect(&info).ok();
        let _ = DeleteObject(color.into());
        let _ = DeleteObject(mask.into());
        icon
    }
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
            state.readded.store(true, Ordering::Relaxed);
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
        let readded = Arc::new(AtomicBool::new(false));
        let readded_too = readded.clone();
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
                    readded: readded_too,
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
        let size = unsafe { GetSystemMetrics(SM_CXSMICON) };
        Some(Tray {
            hwnd,
            events: rx,
            meter: RefCell::new(Meter {
                logo: logo_pixels(size).unwrap_or_default(),
                size: size.max(0) as usize,
                shown: None,
                icon: HICON::default(),
            }),
            readded,
        })
    }

    /// The CPU meter at `cpu` percent, or the plain logo for None. Redrawn
    /// only when the bar moves a step.
    pub fn cpu(&self, cpu: Option<f32>) {
        let mut meter = self.meter.borrow_mut();
        if meter.logo.is_empty() {
            return;
        }
        let want = cpu.map(|c| (crate::meter::level(c, meter.size), c >= 90.0));
        if self.readded.swap(false, Ordering::Relaxed) {
            meter.shown = None;
            if want.is_none() {
                return;
            }
        }
        if want == meter.shown {
            return;
        }
        let pixels = match want {
            Some((level, hot)) => crate::meter::draw(&meter.logo, meter.size, level, hot),
            None => meter.logo.clone(),
        };
        let Some(icon) = icon_from(&pixels, meter.size as i32) else {
            return;
        };
        let mut d = data(self.hwnd());
        d.uFlags = NIF_ICON;
        d.hIcon = icon;
        unsafe {
            let _ = Shell_NotifyIconW(NIM_MODIFY, &d);
            if !meter.icon.is_invalid() {
                let _ = DestroyIcon(meter.icon);
            }
        }
        meter.icon = icon;
        meter.shown = want;
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
            let icon = self.meter.get_mut().icon;
            if !icon.is_invalid() {
                let _ = DestroyIcon(icon);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_meter_icon_shows_what_was_drawn() {
        let size = 16;
        // A half-transparent orange square for the logo.
        let logo: Vec<u8> = (0..size * size).flat_map(|_| [255, 106, 61, 128]).collect();
        let drawn = crate::meter::draw(&logo, size, 8, false);
        let icon = icon_from(&drawn, size as i32).expect("an icon");
        let back = icon_pixels(icon, size as i32).expect("its pixels");
        unsafe {
            let _ = DestroyIcon(icon);
        }
        assert_eq!(back, drawn);
    }
}

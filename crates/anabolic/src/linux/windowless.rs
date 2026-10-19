//! The window on Wayland, where it can be closed but not hidden: winit has
//! no way to take a window off the screen there. Closing into the
//! notification area closes eframe's window for real; Anabolic goes on
//! without one (the history, rules, traces and the MCP server keep running)
//! and opens a new window when the icon, the hotkey or another start asks.
//!
//! Background threads keep the egui context of the window that was up when
//! they started. Their wake-ups come here and go to the window up now, or
//! to the loop that runs while there is none.

use std::cell::{RefCell, RefMut};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::Duration;

use eframe::egui;
use raw_window_handle::{HasDisplayHandle, RawDisplayHandle};

use crate::app::App;

/// The context of the window on screen; none while there is no window.
static WINDOW: Mutex<Option<egui::Context>> = Mutex::new(None);

/// What the loop without a window was woken for.
struct Calls {
    wake: bool,
    show: bool,
}

static CALLS: Mutex<Calls> = Mutex::new(Calls {
    wake: false,
    show: false,
});
static CALLED: Condvar = Condvar::new();

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// A window is up: wake-ups go to it.
pub fn attach(ctx: &egui::Context) {
    *lock(&WINDOW) = Some(ctx.clone());
}

/// The window closed. What asked for one before does not count.
fn detach() {
    *lock(&WINDOW) = None;
    let mut calls = lock(&CALLS);
    calls.wake = false;
    calls.show = false;
}

/// The window on screen, if there is one.
pub fn window() -> Option<egui::Context> {
    lock(&WINDOW).clone()
}

/// Wakes the window, or the loop while there is none.
pub fn repaint() {
    match window() {
        Some(ctx) => ctx.request_repaint(),
        None => call(false),
    }
}

/// Asks for a window while there is none.
pub fn show() {
    call(true);
}

fn call(show: bool) {
    let mut calls = lock(&CALLS);
    calls.wake = true;
    calls.show |= show;
    CALLED.notify_one();
}

/// Waits to be woken, or `at_most`; says whether a window was asked for.
fn wait(at_most: Duration) -> bool {
    let mut calls = lock(&CALLS);
    if !calls.wake {
        calls = CALLED
            .wait_timeout(calls, at_most)
            .map_or_else(|e| e.into_inner().0, |(calls, _)| calls);
    }
    calls.wake = false;
    std::mem::take(&mut calls.show)
}

/// Whether eframe's window is a Wayland one.
pub fn on_wayland(cc: &eframe::CreationContext<'_>) -> bool {
    cc.display_handle()
        .is_ok_and(|h| matches!(h.as_raw(), RawDisplayHandle::Wayland(_)))
}

/// Whether eframe will draw on Wayland, as winit decides it.
pub fn wayland_likely() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some() || std::env::var_os("WAYLAND_SOCKET").is_some()
}

/// Runs eframe's window, and again each time it was closed into the
/// notification area and is asked for. `first` is the first window's
/// options, `options` every later one's; `make` makes the App once.
pub fn run(
    first: eframe::NativeOptions,
    options: eframe::NativeOptions,
    make: impl FnOnce(&eframe::CreationContext<'_>) -> App,
) -> eframe::Result {
    let saved = options.persistence_path.clone();
    let slot: Rc<RefCell<Option<App>>> = Rc::default();
    let mut make = Some(make);
    let mut window = first;
    loop {
        let held = slot.clone();
        let make = &mut make;
        eframe::run_native(
            "Anabolic",
            window,
            Box::new(move |cc| {
                {
                    let mut app = held.borrow_mut();
                    match app.as_mut() {
                        Some(app) => app.reopened(cc),
                        None => *app = make.take().map(|make| make(cc)),
                    }
                }
                Ok(Box::new(Held(held)))
            }),
        )?;
        let mut app = slot.borrow_mut();
        let Some(app) = app.as_mut().filter(|app| app.away()) else {
            return Ok(());
        };
        detach();
        if !app.run_away(wait) {
            // Quit without a window: what eframe does when its own closes.
            if let Some(path) = &saved {
                app.quit_away(&mut Saved::open(path.clone()));
            }
            return Ok(());
        }
        window = options.clone();
    }
}

/// The App, lent to each window in turn.
struct Held(Rc<RefCell<Option<App>>>);

impl Held {
    fn app(&self) -> RefMut<'_, App> {
        RefMut::map(self.0.borrow_mut(), |app| {
            app.as_mut().expect("made before its window")
        })
    }
}

impl eframe::App for Held {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        eframe::App::ui(&mut *self.app(), ui, frame);
    }

    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        eframe::App::logic(&mut *self.app(), ctx, frame);
    }

    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        eframe::App::raw_input_hook(&mut *self.app(), ctx, raw_input);
    }

    fn on_exit(&mut self, gl: Option<&eframe::glow::Context>) {
        eframe::App::on_exit(&mut *self.app(), gl);
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::App::save(&mut *self.app(), storage);
    }
}

/// eframe's file of saved state, for saving without a window (eframe saves
/// only while its window is up). What eframe keeps there, the window's
/// place and egui's memory, stays as it is.
struct Saved {
    path: PathBuf,
    kv: HashMap<String, String>,
}

impl Saved {
    fn open(path: PathBuf) -> Self {
        let kv = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| ron::from_str(&text).ok())
            .unwrap_or_default();
        Self { path, kv }
    }
}

impl eframe::Storage for Saved {
    fn get_string(&self, key: &str) -> Option<String> {
        self.kv.get(key).cloned()
    }

    fn set_string(&mut self, key: &str, value: String) {
        self.kv.insert(key.to_string(), value);
    }

    fn remove_string(&mut self, key: &str) {
        self.kv.remove(key);
    }

    fn flush(&mut self) {
        if let Ok(text) = ron::ser::to_string_pretty(&self.kv, Default::default()) {
            let _ = std::fs::write(&self.path, text);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wakes_and_asks_for_a_window() {
        detach();
        assert!(!wait(Duration::from_millis(1)));
        repaint();
        assert!(!wait(Duration::from_secs(5)));
        show();
        assert!(wait(Duration::from_secs(5)));
        // Asked before the window closed: forgotten with it.
        show();
        detach();
        assert!(!wait(Duration::from_millis(1)));
    }

    #[test]
    fn keeps_what_eframe_saved() {
        let dir = std::env::temp_dir().join(format!("anabolic-saved-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("app.ron");
        std::fs::write(&path, "{\"egui\":\"(x: 1)\",\"app\":\"(old: true)\"}").unwrap();
        let mut saved = Saved::open(path.clone());
        eframe::Storage::set_string(&mut saved, "app", "(new: true)".into());
        eframe::Storage::flush(&mut saved);
        let again = Saved::open(path);
        assert_eq!(again.kv["egui"], "(x: 1)");
        assert_eq!(again.kv["app"], "(new: true)");
        let _ = std::fs::remove_dir_all(dir);
    }
}

//! A key combination that brings Anabolic to the front from any program,
//! for as long as it runs (in the notification area too). Windows posts it
//! to the thread that registered it, so a thread of its own holds it.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use crossbeam_channel::{Receiver, bounded};
use windows::Win32::Foundation::{ERROR_HOTKEY_ALREADY_REGISTERED, LPARAM, WPARAM};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    HOT_KEY_MODIFIERS, MOD_NOREPEAT, RegisterHotKey, UnregisterHotKey,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetMessageW, MSG, PostThreadMessageW, WM_HOTKEY, WM_QUIT,
};

/// A registered combination; dropping it gives it back to Windows.
pub struct Hotkey {
    stop: Arc<AtomicBool>,
    thread: Arc<AtomicU32>,
    answer: Receiver<Result<(), String>>,
}

impl Hotkey {
    /// Asks Windows for `modifiers` (MOD_*) with the virtual key `key`;
    /// `pressed` runs on the hotkey's thread each time it is pressed. Whether
    /// Windows gave it comes later, from `answer`: another program may hold
    /// the combination already.
    pub fn register(modifiers: u32, key: u32, pressed: impl Fn() + Send + 'static) -> Hotkey {
        let stop = Arc::new(AtomicBool::new(false));
        let thread = Arc::new(AtomicU32::new(0));
        let (tx, answer) = bounded(1);
        let (stopped, id) = (stop.clone(), thread.clone());
        let tx_spawn = tx.clone();
        let spawned = std::thread::Builder::new()
            .name("anabolic-hotkey".into())
            .spawn(move || {
                let registered = unsafe {
                    RegisterHotKey(None, 1, HOT_KEY_MODIFIERS(modifiers) | MOD_NOREPEAT, key)
                };
                let _ = tx.send(registered.as_ref().map(|_| ()).map_err(|e| {
                    if e.code() == ERROR_HOTKEY_ALREADY_REGISTERED.to_hresult() {
                        "another program uses it".to_string()
                    } else {
                        e.message()
                    }
                }));
                if registered.is_err() {
                    return;
                }
                id.store(unsafe { GetCurrentThreadId() }, Ordering::SeqCst);
                // Dropped before the id was known: nobody will post WM_QUIT.
                if !stopped.load(Ordering::SeqCst) {
                    let mut msg = MSG::default();
                    while unsafe { GetMessageW(&mut msg, None, 0, 0) }.0 > 0 {
                        if msg.message == WM_HOTKEY {
                            pressed();
                        }
                    }
                }
                unsafe {
                    let _ = UnregisterHotKey(None, 1);
                }
            });
        if let Err(e) = spawned {
            let _ = tx_spawn.send(Err(e.to_string()));
        }
        Hotkey {
            stop,
            thread,
            answer,
        }
    }

    /// Windows' answer to the registration, once: None until it has come.
    pub fn answer(&self) -> Option<Result<(), String>> {
        self.answer.try_recv().ok()
    }
}

impl Drop for Hotkey {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let thread = self.thread.load(Ordering::SeqCst);
        if thread != 0 {
            unsafe {
                let _ = PostThreadMessageW(thread, WM_QUIT, WPARAM(0), LPARAM(0));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hotkey::*;
    use std::time::{Duration, Instant};

    fn wait(key: &Hotkey) -> Result<(), String> {
        let started = Instant::now();
        loop {
            if let Some(answer) = key.answer() {
                return answer;
            }
            assert!(started.elapsed() < Duration::from_secs(5), "no answer");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Ctrl+Alt+Shift+F24, which no keyboard has: taken while held, free
    /// again once dropped.
    #[test]
    fn a_combination_is_held_until_dropped() {
        const F24: u32 = 0x87;
        let mods = MOD_CONTROL | MOD_ALT | MOD_SHIFT;
        let first = Hotkey::register(mods, F24, || {});
        assert_eq!(wait(&first), Ok(()));
        let second = Hotkey::register(mods, F24, || {});
        assert_eq!(wait(&second), Err("another program uses it".to_string()));
        drop(first);
        let started = Instant::now();
        loop {
            let again = Hotkey::register(mods, F24, || {});
            if wait(&again).is_ok() {
                break;
            }
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "never given back"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

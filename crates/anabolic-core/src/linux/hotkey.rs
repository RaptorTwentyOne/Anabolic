//! A shortcut that brings Anabolic to the front from any program.

use crossbeam_channel::{Receiver, bounded};

/// A registered combination.
pub struct Hotkey {
    answer: Receiver<Result<(), String>>,
}

impl Hotkey {
    pub fn register(_modifiers: u32, _key: u32, _pressed: impl Fn() + Send + 'static) -> Hotkey {
        let (tx, answer) = bounded(1);
        let _ = tx.send(Err("global shortcuts are not available on Linux yet".into()));
        Hotkey { answer }
    }

    /// The desktop's answer to the registration, once.
    pub fn answer(&self) -> Option<Result<(), String>> {
        self.answer.try_recv().ok()
    }
}

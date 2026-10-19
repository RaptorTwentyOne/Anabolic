//! The notification area icon on Linux.

use eframe::egui;

pub enum TrayEvent {
    /// The window was brought back.
    Open,
    Quit,
}

pub struct Tray {
    events: std::sync::mpsc::Receiver<TrayEvent>,
}

impl Tray {
    /// None: there is no notification area to put an icon in, so closing
    /// the window must not hide it.
    pub fn start(_ctx: egui::Context) -> Option<Tray> {
        None
    }

    pub fn events(&self) -> impl Iterator<Item = TrayEvent> + '_ {
        self.events.try_iter()
    }

    /// The text shown when the pointer rests on the icon.
    pub fn tip(&self, _text: &str) {}

    /// A notification from the icon.
    pub fn notify(&self, _title: &str, _text: &str, _warning: bool) {}
}

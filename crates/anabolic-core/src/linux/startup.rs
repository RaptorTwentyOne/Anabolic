//! What starts with the session on Linux.

use super::*;

pub(super) fn thread_init() {}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Source {
    /// ~/.config/autostart: this user only.
    AutostartUser,
    /// /etc/xdg/autostart: every user.
    AutostartSystem,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Source::AutostartUser => "Autostart (user)",
            Source::AutostartSystem => "Autostart (all users)",
        }
    }

    pub fn is_task(self) -> bool {
        false
    }

    pub fn machine_wide(self) -> bool {
        self == Source::AutostartSystem
    }
}

pub fn list() -> StartupList {
    StartupList {
        errors: vec!["not available on Linux yet".into()],
        ..Default::default()
    }
}

pub fn set_enabled(_item: &StartupItem, _on: bool) -> Result<String, String> {
    Err("not available on Linux yet".into())
}

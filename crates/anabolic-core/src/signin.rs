//! Starting Anabolic in the notification area when the user signs in.
//!
//! On Windows an administrator's Anabolic runs elevated, and to start that
//! way without asking it is a Task Scheduler task with the highest rights
//! (in the folder \Anabolic), which Windows only allows an elevated
//! program to make. A file started elevated without asking must be where
//! only administrators can change it, or any program could put its own in
//! its place: so only a copy the installer put in Program Files offers it.
//! On a standard account it is the Run key, with no rights to gain. On
//! Linux, an autostart entry.

#[cfg_attr(windows, path = "windows/signin.rs")]
#[cfg_attr(target_os = "linux", path = "linux/signin.rs")]
mod imp;

use std::path::Path;

/// Whether Anabolic starts at sign-in, as far as this copy can tell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SignIn {
    On,
    Off,
    /// It cannot here, and why.
    Unavailable(String),
}

/// What is set up for `exe` (this copy, see `update::this_exe`).
pub fn state(exe: &Path) -> SignIn {
    imp::state(exe)
}

/// Starts `exe` with `--tray` at sign-in, or no longer.
pub fn set(exe: &Path, on: bool) -> Result<(), String> {
    imp::set(exe, on)
}

/// Removes what every user set up: the installer does this as Anabolic is
/// removed (`--forget-sign-in`), so no task is left pointing at nothing.
pub fn forget_everyone() -> Result<(), String> {
    imp::forget_everyone()
}

/// The action thread's part: sets it when `on` says so, then says what is
/// set up for this copy.
pub(crate) fn perform(on: Option<bool>) -> crate::actions::Outcome {
    let Some(exe) = crate::update::this_exe() else {
        return crate::actions::Outcome::SignIn {
            state: SignIn::Unavailable("Anabolic cannot find its own file".into()),
            error: None,
        };
    };
    let error = on.and_then(|on| set(&exe, on).err());
    crate::actions::Outcome::SignIn {
        state: state(&exe),
        error,
    }
}

//! The program in front: which process owns the foreground window, and
//! whether it fills its monitor (a game, full screen).

#[cfg_attr(windows, path = "windows/foreground.rs")]
#[cfg_attr(target_os = "linux", path = "linux/foreground.rs")]
mod imp;
pub use imp::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Front {
    pub pid: u32,
    /// Its window covers its whole monitor.
    pub fullscreen: bool,
}

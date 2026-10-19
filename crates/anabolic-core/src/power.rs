//! Power plans for game mode: the fastest one while a game runs, and the
//! one it replaced afterwards.

#[cfg_attr(windows, path = "windows/power.rs")]
#[cfg_attr(target_os = "linux", path = "linux/power.rs")]
mod imp;
pub use imp::*;

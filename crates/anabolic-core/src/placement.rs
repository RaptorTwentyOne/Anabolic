//! Where Anabolic's window goes: beside a full-screen game rather than over
//! it, and back onto a monitor when it was left on one that is gone.

#[cfg_attr(windows, path = "windows/placement.rs")]
#[cfg_attr(target_os = "linux", path = "linux/placement.rs")]
mod imp;
pub use imp::*;

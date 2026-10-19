//! System-wide memory figures.

#[cfg_attr(windows, path = "windows/memory.rs")]
#[cfg_attr(target_os = "linux", path = "linux/memory.rs")]
mod imp;
pub use imp::*;

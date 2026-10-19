//! Small static facts and conversions the UI needs.

#[cfg_attr(windows, path = "windows/sysinfo.rs")]
#[cfg_attr(target_os = "linux", path = "linux/sysinfo.rs")]
mod imp;
pub use imp::*;

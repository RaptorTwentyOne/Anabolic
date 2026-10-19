//! The names the system's DNS cache holds for addresses: what programs
//! looked up before Anabolic started.

#[cfg_attr(windows, path = "windows/dnscache.rs")]
#[cfg_attr(target_os = "linux", path = "linux/dnscache.rs")]
mod imp;
pub use imp::*;

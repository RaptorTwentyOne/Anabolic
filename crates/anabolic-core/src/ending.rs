//! Why a process does not go away, in the terms of the system it runs on:
//! see each platform's side for what can hold a process back.

#[cfg_attr(windows, path = "windows/ending.rs")]
#[cfg_attr(target_os = "linux", path = "linux/ending.rs")]
mod imp;
pub use imp::*;

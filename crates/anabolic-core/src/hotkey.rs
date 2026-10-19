//! A system-wide shortcut that brings Anabolic to the front from any program.

#[cfg_attr(windows, path = "windows/hotkey.rs")]
#[cfg_attr(target_os = "linux", path = "linux/hotkey.rs")]
mod imp;
pub use imp::*;

pub const MOD_ALT: u32 = 0x1;
pub const MOD_CONTROL: u32 = 0x2;
pub const MOD_SHIFT: u32 = 0x4;

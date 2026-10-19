//! Keeps Anabolic itself responsive when the machine is drowning: high CPU
//! and I/O priority, no power throttling, and memory the system may not
//! take back; and what rights it runs with.

#[cfg_attr(windows, path = "windows/hardening.rs")]
#[cfg_attr(target_os = "linux", path = "linux/hardening.rs")]
mod imp;
pub use imp::*;

/// How a thread of Anabolic's own stands next to its others.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ThreadPriority {
    /// Background work (enrichment, name lookups) that must not slow the rest.
    BelowNormal,
    /// The collector and the traces, which must keep up under load.
    AboveNormal,
}

#[derive(Clone, Debug)]
pub struct HardeningItem {
    pub what: &'static str,
    pub result: Result<(), String>,
}

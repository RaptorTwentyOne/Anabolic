//! Process enumeration: every process with its counters, read in one pass
//! once per refresh. Nothing here opens a process, so nothing can block on a
//! misbehaving one.
//!
//! Each platform's `Entry` gives the collector a [`ProcSample`] per process
//! and, when asked, a [`ThreadSample`] per thread, in the model's units.

use crate::model::{DiskCounters, IoCounters};

#[cfg_attr(windows, path = "windows/scan.rs")]
#[cfg_attr(target_os = "linux", path = "linux/scan.rs")]
mod imp;
pub use imp::*;

/// One process as the scan read it. Times are in 100 ns units, and
/// `create_time` is a FILETIME (100 ns since 1601, UTC).
#[derive(Clone, Copy, Debug, Default)]
pub struct ProcSample {
    pub pid: u32,
    pub ppid: u32,
    pub create_time: i64,
    pub session_id: u32,
    pub base_priority: i32,
    pub threads: u32,
    pub handles: u32,
    pub kernel_time: u64,
    pub user_time: u64,
    pub cycles: u64,
    pub private_bytes: u64,
    pub working_set: u64,
    pub private_working_set: u64,
    pub peak_working_set: u64,
    pub virtual_size: u64,
    pub paged_pool: u64,
    pub nonpaged_pool: u64,
    pub page_faults: u32,
    pub hard_faults: u32,
    pub io: IoCounters,
    /// Storage I/O charged to it, when the system says.
    pub disk: Option<DiskCounters>,
    pub context_switches: u64,
    /// Stopped as a whole (every thread suspended).
    pub suspended: bool,
    /// Ending has begun and not finished (see `ending::why_alive`).
    pub ending: bool,
}

/// One thread as the scan read it, in the units of [`ProcSample`].
#[derive(Clone, Copy, Debug, Default)]
pub struct ThreadSample {
    pub tid: u32,
    pub create_time: i64,
    pub start_address: u64,
    pub win32_start_address: u64,
    pub priority: i32,
    pub base_priority: i32,
    /// See `sysinfo::thread_state_name` and `sysinfo::wait_reason_name`.
    pub state: u32,
    pub wait_reason: u32,
    pub kernel_time: u64,
    pub user_time: u64,
    pub context_switches: u32,
    pub stack_base: u64,
    pub stack_limit: u64,
    pub teb: u64,
}

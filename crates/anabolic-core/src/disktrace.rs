//! Who reads and writes each physical disk, and which files: a kernel trace
//! of disk I/O, attributed to the issuing process.

use std::collections::HashMap;
use std::sync::Arc;

#[cfg_attr(windows, path = "windows/disktrace.rs")]
#[cfg_attr(target_os = "linux", path = "linux/disktrace.rs")]
mod imp;
pub use imp::*;

#[derive(Clone, Copy, Default, Debug)]
pub struct IoTotals {
    pub read: u64,
    pub write: u64,
    pub ops: u32,
    /// Sum of completion times, seconds.
    pub latency: f64,
}

/// One (disk, thread, file object) combination since the previous `take`.
#[derive(Clone, Debug)]
pub struct IoRecord {
    pub disk: u32,
    pub tid: u32,
    /// Kernel address of the file object; 0 for I/O without one.
    pub file_object: u64,
    /// NT path, if the file object has been named.
    pub file: Option<Arc<str>>,
    pub totals: IoTotals,
}

#[derive(Default, Debug)]
pub struct DiskSample {
    pub records: Vec<IoRecord>,
    /// tid -> pid for threads seen starting meanwhile.
    pub owners: HashMap<u32, u32>,
    /// pid -> image name for processes seen starting meanwhile, so the ones
    /// that come and go between two refreshes still have a name.
    pub images: HashMap<u32, Arc<str>>,
    /// Names a rundown found for file objects earlier records left unnamed.
    pub late_names: Vec<(u64, Arc<str>)>,
}

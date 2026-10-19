//! Memory dumps of a process, for a debugger. The process is frozen as
//! briefly as the system allows, and the dump written to a file. Opening a
//! process to read all of its memory is as invasive as inspection gets: the
//! UI asks first.

use std::path::PathBuf;
use std::time::Duration;

#[cfg_attr(windows, path = "windows/dump.rs")]
#[cfg_attr(target_os = "linux", path = "linux/dump.rs")]
mod imp;
pub use imp::*;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DumpKind {
    /// All of its memory: what a debugger needs to look at anything. As
    /// big as the process.
    Full,
    /// Threads, stacks, modules and handles: enough for "where is it stuck".
    Small,
}

impl DumpKind {
    pub fn label(self) -> &'static str {
        match self {
            DumpKind::Full => "full",
            DumpKind::Small => "small",
        }
    }
}

#[derive(Clone, Debug)]
pub struct DumpReport {
    pub path: PathBuf,
    pub bytes: u64,
    pub took: Duration,
    /// Written from a clone; false when Windows would not clone the
    /// process and it was frozen for the whole write.
    pub cloned: bool,
}

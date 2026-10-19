//! One thread of another process: its name, its call stack, and what can
//! be done to it. Every function here opens the thread (and for the stack,
//! the process for reading), so the UI runs them only when asked.

use crate::model::ProcKey;
use std::time::Duration;

#[cfg_attr(windows, path = "windows/threads.rs")]
#[cfg_attr(target_os = "linux", path = "linux/threads.rs")]
mod imp;
pub use imp::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThreadOp {
    Suspend,
    Resume,
    Terminate,
    /// One of the levels in `PRIORITIES`.
    Priority(i32),
}

#[derive(Clone, Debug)]
pub struct Frame {
    pub address: u64,
    /// `module!function+0x12`, or `module+0x1234` without a name.
    pub symbol: Option<String>,
}

#[derive(Clone, Debug)]
pub struct StackReport {
    pub key: ProcKey,
    pub tid: u32,
    /// Innermost first. User mode only: the kernel part is not readable.
    pub frames: Vec<Frame>,
    pub wow64: bool,
    /// How long the thread was held suspended to read it.
    pub held: Duration,
    pub took: Duration,
}

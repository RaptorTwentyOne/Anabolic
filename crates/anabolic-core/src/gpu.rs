//! GPU usage per adapter and engine, and per process.

use crate::model::ProcKey;
use std::collections::HashMap;
use std::sync::Arc;

#[cfg_attr(windows, path = "windows/gpu.rs")]
#[cfg_attr(target_os = "linux", path = "linux/gpu.rs")]
mod imp;
pub use imp::*;

#[derive(Clone, Debug)]
pub struct EngineStats {
    pub name: Arc<str>,
    /// Busy share of the interval, 0..=100.
    pub usage: f32,
}

#[derive(Clone, Debug, Default)]
pub struct AdapterGpuStats {
    pub name: Arc<str>,
    /// The busiest engine, which is what Task Manager reports as "GPU".
    pub usage: f32,
    pub engines: Vec<EngineStats>,
    pub dedicated_used: u64,
    pub dedicated_total: u64,
    pub shared_used: u64,
    pub shared_total: u64,
}

/// GPU share per process (busiest engine), published by its own thread.
pub type GpuProcessMap = HashMap<ProcKey, f32>;

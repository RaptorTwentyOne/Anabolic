//! How long drivers keep the processors from running programs: interrupt
//! service routines and deferred work, per driver, in the manner of
//! LatencyMon.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

#[cfg_attr(windows, path = "windows/latency.rs")]
#[cfg_attr(target_os = "linux", path = "linux/latency.rs")]
mod imp;
pub use imp::*;

/// Upper bounds of the histogram's buckets in ms; the last bucket is open.
pub const BUCKETS_MS: [f64; 4] = [0.1, 0.5, 1.0, 2.0];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    /// Deferred procedure call, threaded and timer ones included.
    Dpc,
    /// Interrupt service routine.
    Isr,
}

/// One kind of work a driver did.
#[derive(Clone, Debug, Default)]
pub struct Activity {
    /// Since tracing started.
    pub count: u64,
    /// Over the last refresh.
    pub per_sec: f32,
    /// Share of one logical processor over the last refresh.
    pub busy: f32,
    /// The longest single one since tracing started, in µs.
    pub highest_us: f32,
    /// The longest over the last refresh, in µs.
    pub recent_highest_us: f32,
    /// Since tracing started, by length: under 0.1 ms, 0.5, 1, 2 ms, longer.
    pub histogram: [u64; 5],
}

#[derive(Clone, Debug)]
pub struct DriverLatency {
    /// The driver's file name (`nvlddmkm.sys`), or "unknown".
    pub driver: Arc<str>,
    pub path: Arc<str>,
    pub dpc: Activity,
    pub isr: Activity,
}

impl DriverLatency {
    pub fn highest_us(&self) -> f32 {
        self.dpc.highest_us.max(self.isr.highest_us)
    }
}

/// A DPC or ISR longer than a millisecond.
#[derive(Clone, Debug)]
pub struct LatencySpike {
    /// When it ended.
    pub at: SystemTime,
    pub driver: Arc<str>,
    pub kind: Kind,
    pub cpu: u16,
    pub us: f32,
}

#[derive(Clone, Debug, Default)]
pub struct LatencyStats {
    /// How long tracing has run.
    pub measured: Duration,
    /// The longest single one first.
    pub drivers: Vec<DriverLatency>,
    /// Per logical processor, the longest DPC or ISR since tracing started (µs).
    pub per_cpu_highest_us: Vec<f32>,
    /// Newest last.
    pub spikes: Vec<LatencySpike>,
    /// No driver addresses could be read (Windows hides them without
    /// administrator rights), so nothing is named.
    pub unnamed: bool,
    /// Events the session dropped so far (it produced faster than we read).
    pub lost_events: u32,
    /// Events whose start time could not be right (zero, from before the
    /// trace began, or longer than any DPC can run), left out.
    pub discarded: u64,
}

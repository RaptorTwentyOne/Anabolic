//! Interrupt handler and softirq time per driver.

use super::*;

pub struct LatencyTracer {
    _private: (),
}

impl LatencyTracer {
    pub fn start() -> Result<Self, String> {
        Err("not available on Linux yet".into())
    }

    pub fn take(&mut self) -> LatencyStats {
        LatencyStats::default()
    }
}

// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Per logical processor time accounting.

use crate::model::CpuTimes;

#[cfg_attr(windows, path = "windows/cpu.rs")]
#[cfg_attr(target_os = "linux", path = "linux/cpu.rs")]
mod imp;
pub use imp::*;

pub struct CpuSample {
    pub total: CpuTimes,
    pub cores: Vec<CpuTimes>,
    /// Sum of (kernel + user) time across all processors during the interval,
    /// in 100 ns units. Process CPU usage is expressed against this.
    pub elapsed_cpu_time: u64,
}

fn times(time: u64, idle: u64, kernel: u64, user: u64, dpc: u64, int: u64) -> CpuTimes {
    if time == 0 {
        return CpuTimes::default();
    }
    let pct = |v: u64| (v as f64 * 100.0 / time as f64).clamp(0.0, 100.0) as f32;
    CpuTimes {
        total: pct(time.saturating_sub(idle)),
        kernel: pct(kernel),
        user: pct(user),
        dpc: pct(dpc),
        interrupt: pct(int),
        interrupt_rate: 0.0,
        context_switch_rate: 0.0,
        dpc_rate: 0.0,
        mhz: 0.0,
    }
}

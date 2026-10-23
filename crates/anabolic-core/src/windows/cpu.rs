// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Per logical processor time accounting, across all processor groups.

use core::mem::size_of;

use windows::Win32::System::Threading::{GetActiveProcessorCount, GetActiveProcessorGroupCount};

use crate::model::CpuTimes;
use crate::ntapi::{self, SYSTEM_INTERRUPT_INFORMATION, SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION};

use super::*;

pub struct CpuSampler {
    group_sizes: Vec<u32>,
    prev: Vec<SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION>,
    cur: Vec<SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION>,
    int_prev: Vec<SYSTEM_INTERRUPT_INFORMATION>,
    int_cur: Vec<SYSTEM_INTERRUPT_INFORMATION>,
    primed: bool,
    last: Option<std::time::Instant>,
    /// Nominal MHz per processor and the previous performance-state hit counts.
    nominal: Vec<u32>,
    freq_prev: Vec<Vec<(u64, u8)>>,
}

impl Default for CpuSampler {
    fn default() -> Self {
        Self::new()
    }
}

impl CpuSampler {
    pub fn new() -> Self {
        let groups = unsafe { GetActiveProcessorGroupCount() }.max(1);
        let group_sizes: Vec<u32> = (0..groups)
            .map(|g| unsafe { GetActiveProcessorCount(g) })
            .collect();
        let total = group_sizes.iter().sum::<u32>().max(1) as usize;
        Self {
            group_sizes,
            prev: vec![Default::default(); total],
            cur: vec![Default::default(); total],
            int_prev: vec![Default::default(); total],
            int_cur: vec![Default::default(); total],
            primed: false,
            last: None,
            nominal: crate::hwinfo::nominal_mhz(total),
            freq_prev: Vec::new(),
        }
    }

    /// Per processor: (hits, percent of nominal frequency) for each
    /// performance state. Windows' "% Processor Performance" comes from this.
    fn performance_states() -> Vec<Vec<(u64, u8)>> {
        let mut buf = vec![0u64; 16384];
        let mut returned = 0u32;
        let status = unsafe {
            ntapi::NtQuerySystemInformation(
                ntapi::SystemProcessorPerformanceDistribution,
                buf.as_mut_ptr().cast(),
                (buf.len() * 8) as u32,
                &mut returned,
            )
        };
        if !ntapi::nt_success(status) {
            return Vec::new();
        }
        let b = unsafe {
            std::slice::from_raw_parts(
                buf.as_ptr().cast::<u8>(),
                (returned as usize).min(buf.len() * 8),
            )
        };
        let u32_at = |at: usize| {
            b.get(at..at + 4)
                .map(|x| u32::from_le_bytes(x.try_into().unwrap()))
        };
        let Some(count) = u32_at(0) else {
            return Vec::new();
        };
        (0..count as usize)
            .map(|i| {
                let Some(offset) = u32_at(4 + i * 4).map(|o| o as usize) else {
                    return Vec::new();
                };
                let states = u32_at(offset + 4).unwrap_or(0) as usize;
                // SYSTEM_PROCESSOR_PERFORMANCE_HITCOUNT: u64 hits, u8 percent, padding.
                (0..states)
                    .filter_map(|s| {
                        let at = offset + 8 + s * 16;
                        let hits = u64::from_le_bytes(b.get(at..at + 8)?.try_into().ok()?);
                        Some((hits, *b.get(at + 8)?))
                    })
                    .collect()
            })
            .collect()
    }

    pub fn core_count(&self) -> usize {
        self.cur.len()
    }

    /// Fills `out` with one `T` per logical processor, group by group.
    fn read_per_cpu<T>(group_sizes: &[u32], class: u32, out: &mut [T]) -> bool {
        let mut offset = 0;
        for (group, &count) in group_sizes.iter().enumerate() {
            let group = group as u16;
            let Some(slice) = out.get_mut(offset..offset + count as usize) else {
                return false;
            };
            let mut returned = 0;
            let status = unsafe {
                ntapi::NtQuerySystemInformationEx(
                    class,
                    (&raw const group).cast(),
                    size_of::<u16>() as u32,
                    slice.as_mut_ptr().cast(),
                    size_of_val(slice) as u32,
                    &mut returned,
                )
            };
            if !ntapi::nt_success(status) {
                return false;
            }
            offset += count as usize;
        }
        true
    }

    fn read(&mut self) -> bool {
        // Interrupt counters are a nicety; the times are what matter.
        if !Self::read_per_cpu(
            &self.group_sizes,
            ntapi::SystemInterruptInformation,
            &mut self.int_cur,
        ) {
            self.int_cur.fill(Default::default());
        }
        Self::read_per_cpu(
            &self.group_sizes,
            ntapi::SystemProcessorPerformanceInformation,
            &mut self.cur,
        )
    }

    pub fn sample(&mut self) -> CpuSample {
        std::mem::swap(&mut self.prev, &mut self.cur);
        std::mem::swap(&mut self.int_prev, &mut self.int_cur);
        let ok = self.read();
        let primed = std::mem::replace(&mut self.primed, ok);
        let now_at = std::time::Instant::now();
        let secs = self
            .last
            .replace(now_at)
            .map_or(1.0, |t| (now_at - t).as_secs_f32().max(1e-3));
        let mut interrupts_sum = 0u64;
        let mut cores = Vec::with_capacity(self.cur.len());
        let (mut idle_sum, mut kernel_sum, mut user_sum, mut dpc_sum, mut int_sum, mut time_sum) =
            (0u64, 0u64, 0u64, 0u64, 0u64, 0u64);

        let (mut switches_sum, mut dpcs_sum) = (0u64, 0u64);
        for (i, (now, before)) in self.cur.iter().zip(&self.prev).enumerate() {
            if !ok || !primed {
                cores.push(CpuTimes::default());
                continue;
            }
            let delta = |a: i64, b: i64| a.saturating_sub(b).max(0) as u64;
            let idle = delta(now.IdleTime, before.IdleTime);
            let kernel_with_idle = delta(now.KernelTime, before.KernelTime);
            let user = delta(now.UserTime, before.UserTime);
            let dpc = delta(now.DpcTime, before.DpcTime);
            let int = delta(now.InterruptTime, before.InterruptTime);
            let time = kernel_with_idle + user;
            let kernel = kernel_with_idle.saturating_sub(idle);
            let interrupts = now.InterruptCount.wrapping_sub(before.InterruptCount) as u64;
            interrupts_sum += interrupts;
            let mut core = times(time, idle, kernel, user, dpc, int);
            core.interrupt_rate = interrupts as f32 / secs;
            let (a, b) = (self.int_cur[i], self.int_prev[i]);
            let switches = a.ContextSwitches.wrapping_sub(b.ContextSwitches) as u64;
            let dpcs = a.DpcCount.wrapping_sub(b.DpcCount) as u64;
            switches_sum += switches;
            dpcs_sum += dpcs;
            core.context_switch_rate = switches as f32 / secs;
            core.dpc_rate = dpcs as f32 / secs;
            cores.push(core);
            idle_sum += idle;
            kernel_sum += kernel;
            user_sum += user;
            dpc_sum += dpc;
            int_sum += int;
            time_sum += time;
        }

        let mut total = times(time_sum, idle_sum, kernel_sum, user_sum, dpc_sum, int_sum);
        total.interrupt_rate = interrupts_sum as f32 / secs;
        total.context_switch_rate = switches_sum as f32 / secs;
        total.dpc_rate = dpcs_sum as f32 / secs;

        // Effective clock: hit-weighted performance percent times nominal.
        let states = Self::performance_states();
        if states.len() == cores.len() && self.freq_prev.len() == states.len() {
            let (mut sum, mut n) = (0.0f32, 0);
            for (i, (now, before)) in states.iter().zip(&self.freq_prev).enumerate() {
                let (mut hits, mut weighted) = (0u64, 0u64);
                for (&(h1, pct), &(h0, _)) in now.iter().zip(before) {
                    let d = h1.saturating_sub(h0);
                    hits += d;
                    weighted += d * pct as u64;
                }
                if hits > 0 {
                    let mhz = weighted as f32 / hits as f32 / 100.0
                        * self.nominal.get(i).copied().unwrap_or(0) as f32;
                    cores[i].mhz = mhz;
                    sum += mhz;
                    n += 1;
                }
            }
            total.mhz = if n > 0 { sum / n as f32 } else { 0.0 };
        }
        self.freq_prev = states;
        CpuSample {
            total,
            cores,
            elapsed_cpu_time: time_sum,
        }
    }
}

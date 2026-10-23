// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Per logical processor time accounting from /proc/stat, with interrupt,
//! softirq and scheduling counts and the clock each core runs at.

use std::time::Instant;

use crate::model::CpuTimes;
use crate::procfs;

use super::*;

/// One processor's line of /proc/stat, in clock ticks.
#[derive(Clone, Copy, Default)]
struct Times {
    user: u64,
    nice: u64,
    system: u64,
    idle: u64,
    iowait: u64,
    irq: u64,
    softirq: u64,
    steal: u64,
}

impl Times {
    fn total(&self) -> u64 {
        self.user
            + self.nice
            + self.system
            + self.idle
            + self.iowait
            + self.irq
            + self.softirq
            + self.steal
    }
}

/// Counters per processor that only ever grow.
#[derive(Clone, Copy, Default)]
struct Counts {
    interrupts: u64,
    softirqs: u64,
    switches: u64,
}

pub struct CpuSampler {
    /// The processor number of each /proc/stat line, in order.
    cpus: Vec<usize>,
    prev: Vec<Times>,
    counts: Vec<Counts>,
    primed: bool,
    last: Option<Instant>,
}

impl Default for CpuSampler {
    fn default() -> Self {
        Self::new()
    }
}

/// The per-processor lines of /proc/stat: (processor, times).
fn read_stat() -> Vec<(usize, Times)> {
    let Ok(text) = std::fs::read_to_string("/proc/stat") else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|l| {
            let rest = l.strip_prefix("cpu")?;
            let (number, values) = rest.split_once(' ')?;
            let cpu: usize = number.parse().ok()?;
            let v: Vec<u64> = values
                .split_ascii_whitespace()
                .map(|x| x.parse().unwrap_or(0))
                .collect();
            let at = |i: usize| v.get(i).copied().unwrap_or(0);
            Some((
                cpu,
                Times {
                    user: at(0),
                    nice: at(1),
                    system: at(2),
                    idle: at(3),
                    iowait: at(4),
                    irq: at(5),
                    softirq: at(6),
                    steal: at(7),
                },
            ))
        })
        .collect()
}

/// Per processor: a table whose first line names the processors
/// (`CPU0 CPU1 …`) and whose rows count per processor, summed.
fn per_cpu_sums(path: &str, cpus: &[usize]) -> Vec<u64> {
    let mut sums = vec![0u64; cpus.len()];
    let Ok(text) = std::fs::read_to_string(path) else {
        return sums;
    };
    let mut lines = text.lines();
    let Some(header) = lines.next() else {
        return sums;
    };
    // Column i is the processor numbered in the header.
    let columns: Vec<Option<usize>> = header
        .split_ascii_whitespace()
        .map(|c| {
            let n: usize = c.strip_prefix("CPU")?.parse().ok()?;
            cpus.iter().position(|&cpu| cpu == n)
        })
        .collect();
    for line in lines {
        let mut fields = line.split_ascii_whitespace();
        // The row's name ("0:", "NMI:", "TIMER:").
        fields.next();
        for (column, value) in columns.iter().zip(fields) {
            let (Some(at), Ok(n)) = (column, value.parse::<u64>()) else {
                continue;
            };
            sums[*at] += n;
        }
    }
    sums
}

/// Context switches per processor: the scheduler's statistics count every
/// call into it.
fn switches(cpus: &[usize]) -> Vec<u64> {
    let mut out = vec![0u64; cpus.len()];
    let Ok(text) = std::fs::read_to_string("/proc/schedstat") else {
        return out;
    };
    for line in text.lines() {
        let Some(rest) = line.strip_prefix("cpu") else {
            continue;
        };
        let mut fields = rest.split_ascii_whitespace();
        let Some(cpu) = fields.next().and_then(|n| n.parse::<usize>().ok()) else {
            continue;
        };
        // yld_count, legacy, sched_count.
        let Some(count) = fields.nth(2).and_then(|n| n.parse::<u64>().ok()) else {
            continue;
        };
        if let Some(at) = cpus.iter().position(|&c| c == cpu) {
            out[at] = count;
        }
    }
    out
}

/// The clock each processor ran at, in MHz: the average since the last
/// look where the kernel keeps one (cpuinfo_avg_freq), else the current.
fn clocks(cpus: &[usize]) -> Vec<f32> {
    cpus.iter()
        .map(|cpu| {
            let base = format!("/sys/devices/system/cpu/cpu{cpu}/cpufreq");
            ["cpuinfo_avg_freq", "scaling_cur_freq"]
                .iter()
                .find_map(|file| {
                    std::fs::read_to_string(format!("{base}/{file}"))
                        .ok()?
                        .trim()
                        .parse::<f32>()
                        .ok()
                })
                .map_or(0.0, |khz| khz / 1000.0)
        })
        .collect()
}

impl CpuSampler {
    pub fn new() -> Self {
        let lines = read_stat();
        let cpus: Vec<usize> = if lines.is_empty() {
            vec![0]
        } else {
            lines.iter().map(|(cpu, _)| *cpu).collect()
        };
        Self {
            prev: vec![Times::default(); cpus.len()],
            counts: vec![Counts::default(); cpus.len()],
            cpus,
            primed: false,
            last: None,
        }
    }

    pub fn core_count(&self) -> usize {
        self.cpus.len()
    }

    pub fn sample(&mut self) -> CpuSample {
        let now_at = Instant::now();
        let secs = self
            .last
            .replace(now_at)
            .map_or(1.0, |t| (now_at - t).as_secs_f32().max(1e-3));
        let lines = read_stat();
        let interrupts = per_cpu_sums("/proc/interrupts", &self.cpus);
        let softirqs = per_cpu_sums("/proc/softirqs", &self.cpus);
        let switches = switches(&self.cpus);
        let clocks = clocks(&self.cpus);
        let primed = std::mem::replace(&mut self.primed, !lines.is_empty());

        let mut cores = Vec::with_capacity(self.cpus.len());
        let (mut idle_sum, mut kernel_sum, mut user_sum, mut dpc_sum, mut int_sum, mut time_sum) =
            (0u64, 0u64, 0u64, 0u64, 0u64, 0u64);
        let (mut interrupts_sum, mut switches_sum, mut softirqs_sum) = (0u64, 0u64, 0u64);
        let (mut mhz_sum, mut mhz_n) = (0.0f32, 0);
        for (i, &cpu) in self.cpus.iter().enumerate() {
            let now = lines
                .iter()
                .find(|(c, _)| *c == cpu)
                .map_or(Times::default(), |(_, t)| *t);
            let before = std::mem::replace(&mut self.prev[i], now);
            let counts = Counts {
                interrupts: interrupts[i],
                softirqs: softirqs[i],
                switches: switches[i],
            };
            let counted = std::mem::replace(&mut self.counts[i], counts);
            if !primed {
                cores.push(CpuTimes::default());
                continue;
            }
            let d = |a: u64, b: u64| procfs::ticks_to_100ns(a.saturating_sub(b));
            let time = d(now.total(), before.total());
            let idle = d(now.idle + now.iowait, before.idle + before.iowait);
            let int = d(now.irq, before.irq);
            let dpc = d(now.softirq, before.softirq);
            // Kernel time includes the interrupts and softirqs it served, as
            // Windows' does its interrupts and DPCs.
            let kernel = d(now.system, before.system) + int + dpc;
            let user = d(now.user + now.nice, before.user + before.nice);
            let mut core = times(time, idle, kernel, user, dpc, int);
            let per_sec = |a: u64, b: u64| a.saturating_sub(b) as f32 / secs;
            core.interrupt_rate = per_sec(counts.interrupts, counted.interrupts);
            core.dpc_rate = per_sec(counts.softirqs, counted.softirqs);
            core.context_switch_rate = per_sec(counts.switches, counted.switches);
            core.mhz = clocks[i];
            interrupts_sum += counts.interrupts.saturating_sub(counted.interrupts);
            softirqs_sum += counts.softirqs.saturating_sub(counted.softirqs);
            switches_sum += counts.switches.saturating_sub(counted.switches);
            if clocks[i] > 0.0 {
                mhz_sum += clocks[i];
                mhz_n += 1;
            }
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
        total.dpc_rate = softirqs_sum as f32 / secs;
        total.context_switch_rate = switches_sum as f32 / secs;
        total.mhz = if mhz_n > 0 {
            mhz_sum / mhz_n as f32
        } else {
            0.0
        };
        CpuSample {
            total,
            cores,
            elapsed_cpu_time: time_sum,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_every_processor_and_adds_up() {
        let mut sampler = CpuSampler::new();
        let n = std::thread::available_parallelism().map_or(1, |n| n.get());
        assert_eq!(sampler.core_count(), n);
        sampler.sample();
        std::thread::sleep(std::time::Duration::from_millis(200));
        let s = sampler.sample();
        assert_eq!(s.cores.len(), n);
        assert!(s.elapsed_cpu_time > 0);
        assert!((0.0..=100.0).contains(&s.total.total));
        assert!(s.total.kernel + s.total.user <= s.total.total + 1.0);
    }
}

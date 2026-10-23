// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! How the logical processors are arranged: cores, their caches, and which
//! are performance or efficiency cores.

#[cfg_attr(windows, path = "windows/topology.rs")]
#[cfg_attr(target_os = "linux", path = "linux/topology.rs")]
mod imp;
pub use imp::*;

#[derive(Clone, Debug, Default)]
pub struct Cache {
    pub level: u8,
    pub size: u32,
    /// Logical processors sharing this cache.
    pub cpus: Vec<usize>,
}

#[derive(Clone, Debug, Default)]
pub struct LogicalCpu {
    /// Physical core ordinal.
    pub core: usize,
    /// Other logical processors on the same physical core (SMT).
    pub siblings: Vec<usize>,
    /// Higher is faster on hybrid CPUs; all equal otherwise.
    pub efficiency_class: u8,
    pub numa: u32,
    /// Indices into `Topology::caches`.
    pub l1d: Option<usize>,
    pub l2: Option<usize>,
    pub l3: Option<usize>,
}

#[derive(Clone, Debug, Default)]
pub struct Topology {
    pub cpus: Vec<LogicalCpu>,
    pub caches: Vec<Cache>,
    pub cores: usize,
}

impl Topology {
    /// Which L3 domain (CCD/CCX on AMD) a processor belongs to, by order.
    pub fn l3_domain(&self, cpu: usize) -> Option<usize> {
        let l3 = self.cpus.get(cpu)?.l3?;
        let mut domains: Vec<usize> = self.cpus.iter().filter_map(|c| c.l3).collect();
        domains.sort_unstable();
        domains.dedup();
        domains.iter().position(|&d| d == l3)
    }
}

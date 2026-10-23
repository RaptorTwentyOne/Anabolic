// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! System-wide network throughput, per physical adapter.

use std::collections::HashMap;

#[cfg_attr(windows, path = "windows/net.rs")]
#[cfg_attr(target_os = "linux", path = "linux/net.rs")]
mod imp;

#[derive(Clone, Debug, Default)]
pub struct AdapterStats {
    /// Stable identity (interface LUID).
    pub luid: u64,
    pub name: String,
    pub description: String,
    /// Bits per second, as negotiated.
    pub link_speed: u64,
    pub rx_rate: f64,
    pub tx_rate: f64,
    pub rx_total: u64,
    pub tx_total: u64,
}

#[derive(Clone, Debug, Default)]
pub struct NetStats {
    pub rx_rate: f64,
    pub tx_rate: f64,
    pub adapters: Vec<AdapterStats>,
}

#[derive(Default)]
pub struct NetSampler {
    prev: HashMap<u64, (u64, u64)>,
}

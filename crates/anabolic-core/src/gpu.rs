// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! GPU usage per adapter and engine, and per process.

use crate::model::ProcKey;
use crate::sensors::{Sensor, SensorKind};
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
    /// °C, as its driver reports it; None when it reports none, or the GPU
    /// sleeps and is not woken to be asked.
    pub temperature: Option<f32>,
    /// Everything its driver reports: temperatures with their limits,
    /// fans, power draw.
    pub sensors: Vec<Sensor>,
}

impl AdapterGpuStats {
    /// The temperature sensor `temperature` comes from, with its limits.
    pub fn temperature_sensor(&self) -> Option<&Sensor> {
        self.sensors
            .iter()
            .find(|s| s.kind == SensorKind::Temperature)
    }
}

/// GPU share per process (busiest engine), published by its own thread.
pub type GpuProcessMap = HashMap<ProcKey, f32>;

// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Who runs on each logical processor: a kernel trace samples, many times a
//! second, which thread is on every core.

use std::collections::HashMap;
use std::sync::Arc;

#[cfg_attr(windows, path = "windows/coretrace.rs")]
#[cfg_attr(target_os = "linux", path = "linux/coretrace.rs")]
mod imp;
pub use imp::*;

/// Samples per thread on each processor since the previous `take`, plus the
/// owners of threads seen starting meanwhile.
#[derive(Default, Debug)]
pub struct CoreSamples {
    /// Per processor: (tid, samples); tid 0 is the idle thread.
    pub per_cpu: Vec<Vec<(u32, u32)>>,
    pub owners: HashMap<u32, u32>,
    /// pid -> image name for processes seen starting meanwhile: one that
    /// lives for less than a refresh is never in a process scan.
    pub images: HashMap<u32, Arc<str>>,
}

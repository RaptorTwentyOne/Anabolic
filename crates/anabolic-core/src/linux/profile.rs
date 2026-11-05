// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! The profiler is on Windows only for now: Linux would need perf's call
//! chains and the ELF symbols of every module.

use crate::profile::Profile;

pub fn profile(
    _pid: u32,
    _seconds: f32,
    _stop: &(dyn Fn() -> bool + Sync),
) -> Result<Profile, String> {
    Err("Sampling where a process spends its time is on Windows only for now".into())
}

// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! The code a process (or the kernel) has loaded, and naming addresses in
//! it.

use std::sync::Arc;

#[cfg_attr(windows, path = "windows/modules.rs")]
#[cfg_attr(target_os = "linux", path = "linux/modules.rs")]
mod imp;
pub use imp::*;

#[derive(Clone, Debug)]
pub struct ModuleInfo {
    pub base: u64,
    pub size: u64,
    pub name: Arc<str>,
    pub path: Arc<str>,
}

/// The module containing `address`, by binary search over sorted modules.
pub fn module_at(modules: &[ModuleInfo], address: u64) -> Option<&ModuleInfo> {
    let i = modules
        .partition_point(|m| m.base <= address)
        .checked_sub(1)?;
    let m = &modules[i];
    (address < m.base + m.size).then_some(m)
}

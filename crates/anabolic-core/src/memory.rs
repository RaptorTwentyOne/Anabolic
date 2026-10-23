// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! System-wide memory figures.

#[cfg_attr(windows, path = "windows/memory.rs")]
#[cfg_attr(target_os = "linux", path = "linux/memory.rs")]
mod imp;
pub use imp::*;

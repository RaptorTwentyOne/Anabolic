// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! The names the system's DNS cache holds for addresses: what programs
//! looked up before Anabolic started.

#[cfg_attr(windows, path = "windows/dnscache.rs")]
#[cfg_attr(target_os = "linux", path = "linux/dnscache.rs")]
mod imp;
pub use imp::*;

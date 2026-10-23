// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Where Anabolic's window goes: beside a full-screen game rather than over
//! it, and back onto a monitor when it was left on one that is gone.

#[cfg_attr(windows, path = "windows/placement.rs")]
#[cfg_attr(target_os = "linux", path = "linux/placement.rs")]
mod imp;
pub use imp::*;

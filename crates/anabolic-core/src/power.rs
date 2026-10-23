// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Power plans for game mode: the fastest one while a game runs, and the
//! one it replaced afterwards.

#[cfg_attr(windows, path = "windows/power.rs")]
#[cfg_attr(target_os = "linux", path = "linux/power.rs")]
mod imp;
pub use imp::*;

// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Why a process does not go away, in the terms of the system it runs on:
//! see each platform's side for what can hold a process back.

#[cfg_attr(windows, path = "windows/ending.rs")]
#[cfg_attr(target_os = "linux", path = "linux/ending.rs")]
mod imp;
pub use imp::*;

// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Frame times. Windows' graphics runtimes report every frame a program
//! presents to a system-wide trace; Linux's (Mesa, Vulkan, the
//! compositors) report them to no one outside the program, so frame times
//! cannot be read without injecting into the game, which Anabolic does not
//! do.

use std::sync::Arc;

use super::*;

pub struct FrameTracer {
    _private: (),
}

impl FrameTracer {
    pub fn start(_kernel: bool) -> Result<Self, String> {
        Err(
            "Linux reports no frames system-wide: games' frame times are known only to an \
             overlay inside the game (MangoHud)"
                .into(),
        )
    }

    pub fn take(&mut self, _name_of: impl Fn(u32) -> Option<Arc<str>>) -> FrameStats {
        FrameStats::default()
    }
}

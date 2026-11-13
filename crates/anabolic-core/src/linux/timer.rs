// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Linux runs high-resolution timers for every program: there is no
//! system-wide resolution to raise.

use crate::timer::TimerResolution;

pub fn read() -> Result<TimerResolution, String> {
    Err(
        "Linux has no system-wide timer resolution: its timers are high resolution for every \
program."
            .into(),
    )
}

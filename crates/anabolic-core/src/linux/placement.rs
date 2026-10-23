// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Window placement is the compositor's on Wayland: a program cannot see
//! where other programs' windows are, nor move its own. There is no
//! full-screen game to keep away from as far as Anabolic can tell.

/// The monitor a full-screen game fills: never known here.
pub fn fullscreen_monitor() -> Option<isize> {
    None
}

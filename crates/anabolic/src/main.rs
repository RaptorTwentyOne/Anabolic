// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Anabolic, the window program.

// Debug builds too: started from Explorer or at sign-in, a console program
// would bring a console window along. Run from a terminal, the parent's
// console is attached (see `run`).
#![windows_subsystem = "windows"]

fn main() -> eframe::Result {
    anabolic::run()
}

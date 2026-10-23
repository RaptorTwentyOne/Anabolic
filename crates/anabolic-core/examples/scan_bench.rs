// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Steady-state cost of one NtQuerySystemInformation process scan per class.

#[cfg(windows)]
#[path = "windows/scan_bench.rs"]
mod example;

fn main() {
    #[cfg(windows)]
    example::main();
    #[cfg(not(windows))]
    eprintln!("This example times the Windows native process scan: it runs on Windows only.");
}

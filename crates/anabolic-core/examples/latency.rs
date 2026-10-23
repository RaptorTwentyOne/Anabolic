// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Runs the frame and DPC/ISR traces for a while and prints what they saw,
//! with what tracing cost this process. Needs an elevated prompt.
//!
//! cargo run -p anabolic-core --example latency [seconds] [frames|latency]

#[cfg(windows)]
#[path = "windows/latency.rs"]
mod example;

fn main() {
    #[cfg(windows)]
    example::main();
    #[cfg(not(windows))]
    eprintln!("This example reads Windows' kernel traces: it runs on Windows only.");
}

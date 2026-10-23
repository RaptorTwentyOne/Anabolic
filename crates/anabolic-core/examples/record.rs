// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Records what a process does, Process Monitor style, and prints the
//! timeline. Needs an elevated shell.
//!
//!   record <pid> [secs]                    a running process (and children)
//!   record --all [secs]                    every process (counts only)
//!   record --launch <program> [args...]    start one and record it
//!   record --hidden <program> [args...]    the same, no window (tests)
//!   record --shell-hidden <program> [args...]  through Explorer, hidden
//!
//! Ends after `secs` (default 15), or when a launched program has exited.

#[cfg(windows)]
#[path = "windows/record.rs"]
mod example;

fn main() {
    #[cfg(windows)]
    example::main();
    #[cfg(not(windows))]
    eprintln!("This example records through Windows' kernel traces: it runs on Windows only.");
}

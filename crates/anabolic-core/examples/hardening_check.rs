// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Applies the self-hardening steps and reports which ones took effect.
use anabolic_core::hardening::{harden_process, is_elevated, own_working_set, pin_working_set};

fn main() {
    println!("elevated: {}", is_elevated());
    for item in harden_process() {
        println!("{:36} {:?}", item.what, item.result);
    }
    let ws = own_working_set() as usize;
    println!(
        "working set {} KB, pin -> {:?}",
        ws / 1024,
        pin_working_set(ws + 32 * 1024 * 1024)
    );
}

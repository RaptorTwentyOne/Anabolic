// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! On Linux the kernel uses messages wherever a device and its driver can,
//! and irqbalance or /proc/irq place them: nothing here to set.

use crate::interrupts::{Change, Device};

pub fn read() -> Result<Vec<Device>, String> {
    Err("Linux uses message-signalled interrupts wherever it can.".into())
}

pub fn set(_id: &str, _change: Change) -> Result<String, String> {
    Err("Linux uses message-signalled interrupts wherever it can.".into())
}

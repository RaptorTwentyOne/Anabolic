// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Steady-state cost of one NtQuerySystemInformation process scan per class.
use anabolic_core::ntapi::*;
use std::time::Instant;

pub fn main() {
    let mut buf = vec![0u64; 16 * 1024 * 1024];
    for (name, class) in [("Full", 148u32), ("Extended", 57), ("Basic", 5)] {
        let mut times = Vec::new();
        let mut size = 0;
        for _ in 0..20 {
            let mut needed = 0u32;
            let t = Instant::now();
            let st = unsafe {
                NtQuerySystemInformation(
                    class,
                    buf.as_mut_ptr().cast(),
                    (buf.len() * 8) as u32,
                    &mut needed,
                )
            };
            times.push(t.elapsed().as_secs_f64() * 1000.0);
            assert!(st >= 0, "{name}: {st:#x}");
            size = needed;
        }
        times.sort_by(|a, b| a.partial_cmp(b).unwrap());
        println!(
            "{name:9} min {:6.2} ms  median {:6.2} ms  max {:6.2} ms  ({} KB)",
            times[0],
            times[10],
            times[19],
            size / 1024
        );
    }
}

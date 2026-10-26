// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! The CPU meter in the notification area icon: a bar up the icon's right
//! edge, filled as far as the CPU is busy; amber from 90 %. Drawn into the
//! logo's pixels, the same on Windows and Linux.

use crate::theme;

/// How many steps the bar of an icon `size` pixels high moves in: one a
/// pixel, at most 16, so a big icon is not redrawn for every flicker.
pub fn steps(size: usize) -> usize {
    size.clamp(1, 16)
}

/// The bar's level for `cpu` percent, in `steps(size)` steps.
pub fn level(cpu: f32, size: usize) -> usize {
    let steps = steps(size);
    ((cpu.clamp(0.0, 100.0) / 100.0) * steps as f32).round() as usize
}

/// The logo `rgba` (straight alpha, `size` × `size`) with the meter at
/// `level` (of `steps(size)`) drawn over its right quarter; `hot` in amber.
pub fn draw(rgba: &[u8], size: usize, level: usize, hot: bool) -> Vec<u8> {
    let mut out = rgba.to_vec();
    if size == 0 || rgba.len() < size * size * 4 {
        return out;
    }
    let width = (size / 4).max(3);
    let left = size - width;
    let filled = size * level.min(steps(size)) / steps(size);
    let fill = if hot {
        theme::ELEVATED_TEXT
    } else {
        theme::CPU
    };
    for y in 0..size {
        for x in left..size {
            let i = (y * size + x) * 4;
            // A column of clear space between the logo and the bar.
            let pixel = if x == left {
                [0, 0, 0, 0]
            } else if size - y <= filled {
                [fill.r(), fill.g(), fill.b(), 255]
            } else {
                [theme::BG.r(), theme::BG.g(), theme::BG.b(), 235]
            };
            out[i..i + 4].copy_from_slice(&pixel);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fills_from_the_bottom_as_far_as_the_cpu_goes() {
        let size = 16;
        let logo = vec![255u8; size * size * 4];
        assert_eq!(level(50.0, size), 8);
        assert_eq!(level(150.0, size), 16);
        let half = draw(&logo, size, level(50.0, size), false);
        let at = |img: &[u8], x: usize, y: usize| img[(y * size + x) * 4..][..4].to_vec();
        let cpu = theme::CPU;
        assert_eq!(at(&half, 15, 15), [cpu.r(), cpu.g(), cpu.b(), 255]);
        assert_eq!(at(&half, 15, 8), [cpu.r(), cpu.g(), cpu.b(), 255]);
        assert_ne!(at(&half, 15, 7), [cpu.r(), cpu.g(), cpu.b(), 255]);
        // The gap, and the logo left of it untouched.
        assert_eq!(at(&half, 12, 15)[3], 0);
        assert_eq!(at(&half, 11, 15), [255; 4]);
        let hot = draw(&logo, size, level(95.0, size), true);
        assert_eq!(at(&hot, 15, 15)[0], theme::ELEVATED_TEXT.r());
        assert_eq!(
            at(&hot, 15, 0)[0],
            theme::BG.r(),
            "95 % leaves the top step"
        );
        // A big icon moves in 16 steps.
        assert_eq!(level(50.0, 64), 8);
    }
}

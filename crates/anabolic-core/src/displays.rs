// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Every monitor: where it sits on the desktop, its mode, what it could do
//! and what drives it.

#[cfg_attr(windows, path = "windows/displays.rs")]
#[cfg_attr(target_os = "linux", path = "linux/displays.rs")]
mod imp;
pub use imp::*;

#[derive(Clone, Debug, Default)]
pub struct Display {
    /// The system's name for it: `\\.\DISPLAY3` on Windows, the connector
    /// (`eDP-1`) on Linux.
    pub device: String,
    /// The monitor's own name, from its EDID ("DELL S2721DGF"); empty for
    /// some built-in and virtual ones.
    pub name: String,
    /// The GPU it hangs off.
    pub gpu: String,
    pub connection: &'static str,
    pub primary: bool,
    /// Where it sits on the desktop and its size, in pixels.
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    /// Its refresh rate now, exactly (59.94, 143.98...).
    pub refresh_hz: f64,
    /// The fastest it offers at this resolution, and at any.
    pub max_hz_here: u32,
    pub max_hz: u32,
    /// Its largest mode: usually the panel's own (native) resolution.
    pub native: (u32, u32),
    pub hdr_supported: bool,
    pub hdr_on: bool,
    pub bits_per_color: u32,
    /// The desktop's scaling, percent.
    pub scale_pct: u32,
}

impl Display {
    /// Runs slower than it could at this resolution (a common oversight:
    /// a new monitor left at Windows' 60 Hz default).
    pub fn below_max(&self) -> bool {
        self.max_hz_here > 0 && (self.refresh_hz + 1.5) < self.max_hz_here as f64
    }

    /// Runs below the panel's own resolution: Windows scales the picture
    /// up, and text and edges come out soft.
    pub fn below_native(&self) -> bool {
        self.native.0 > self.width && self.native.1 > self.height
    }
}

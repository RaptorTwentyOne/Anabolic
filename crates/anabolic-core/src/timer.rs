// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! The system timer's resolution: how often Windows' clock interrupt
//! fires, 15.6 ms apart by default and down to 0.5 ms, and which programs
//! asked for a finer one (what TimerResolution tools and `powercfg
//! /energy` show). A finer timer lets sleeps and frame pacing wake on
//! time; it also wakes the processor more often, which costs power and
//! keeps cores from resting deeply. Linux has no such single setting.

use std::time::Duration;

#[cfg_attr(windows, path = "windows/timer.rs")]
#[cfg_attr(target_os = "linux", path = "linux/timer.rs")]
mod imp;
pub use imp::read;

#[derive(Clone, Debug, PartialEq)]
pub struct Request {
    pub pid: u32,
    /// The program's file name.
    pub name: String,
    pub path: Option<String>,
    /// What it asked for, milliseconds.
    pub ms: f64,
}

#[derive(Clone, Debug, Default)]
pub struct TimerResolution {
    /// Now, milliseconds: the finest any program asked for, or the
    /// default.
    pub current_ms: f64,
    pub finest_ms: f64,
    pub coarsest_ms: f64,
    /// Finest first.
    pub requests: Vec<Request>,
    /// Why the requests could not be read, when they could not.
    pub requests_unknown: Option<String>,
    pub took: Duration,
}

impl TimerResolution {
    /// Raised above the default by someone.
    pub fn raised(&self) -> bool {
        self.current_ms + 0.05 < self.coarsest_ms
    }
}

/// 100-nanosecond units as milliseconds.
pub fn ms(units: u32) -> f64 {
    units as f64 / 10_000.0
}

/// "0.5 ms", "15.6 ms".
pub fn words(ms: f64) -> String {
    if ms >= 10.0 {
        format!("{ms:.1} ms")
    } else if (ms - ms.round()).abs() < 0.005 {
        format!("{ms:.0} ms")
    } else {
        let t = format!("{ms:.2}");
        format!("{} ms", t.trim_end_matches('0'))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn says_milliseconds() {
        assert_eq!(words(ms(156_250)), "15.6 ms");
        assert_eq!(words(ms(5_000)), "0.5 ms");
        assert_eq!(words(ms(10_000)), "1 ms");
        assert_eq!(words(ms(100)), "0.01 ms");
    }
}

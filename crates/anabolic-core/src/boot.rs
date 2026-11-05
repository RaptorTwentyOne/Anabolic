// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! The machine's boots: when each began, how long the firmware and then
//! the system took until someone could sign in, what kind of start it was,
//! and whether the shutdown before it was clean. Read from the system's own
//! logs (Windows' event logs, systemd on Linux); nothing is kept by us.

use std::time::Duration;

#[cfg_attr(windows, path = "windows/boot.rs")]
#[cfg_attr(target_os = "linux", path = "linux/boot.rs")]
mod imp;
pub use imp::read;

#[derive(Clone, Debug, Default)]
pub struct Boot {
    /// When the system's kernel started, as a FILETIME.
    pub started: i64,
    pub kind: BootKind,
    /// The firmware's part before the system's loader ran, in ms.
    pub firmware_ms: Option<u64>,
    /// From the kernel's start to the first sign-in (Windows) or to the
    /// graphical target (Linux), in ms. On Windows it includes the time the
    /// sign-in screen waited for someone.
    pub ready_ms: Option<u64>,
    /// How the shutdown before this boot went.
    pub before: Shutdown,
    /// The system's own measurement of the boot, when it kept one.
    pub measured: Option<Measured>,
    /// What the system found slower than usual at this boot, slowest
    /// first; on Linux, the units that took longest.
    pub slowed: Vec<Slow>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BootKind {
    /// Everything started from scratch.
    Cold,
    /// Windows' fast startup: the kernel came back from a hibernation file.
    FastStartup,
    /// Back from hibernation.
    Hibernation,
    #[default]
    Unknown,
}

impl BootKind {
    pub fn label(self) -> &'static str {
        match self {
            BootKind::Cold => "Full start",
            BootKind::FastStartup => "Fast startup",
            BootKind::Hibernation => "From hibernation",
            BootKind::Unknown => "",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Shutdown {
    Clean,
    /// A stop error (blue screen) with its code.
    Crashed(u32),
    /// The power button was held.
    PowerButton,
    /// The system stopped without shutting down: power lost, a hang, a
    /// reset.
    Unexpected,
    #[default]
    Unknown,
}

impl Shutdown {
    pub fn label(self) -> String {
        match self {
            Shutdown::Clean => "Clean".into(),
            Shutdown::Crashed(code) => format!("Stop error 0x{code:X}"),
            Shutdown::PowerButton => "Power button held".into(),
            Shutdown::Unexpected => "Not shut down".into(),
            Shutdown::Unknown => String::new(),
        }
    }

    pub fn bad(self) -> bool {
        matches!(
            self,
            Shutdown::Crashed(_) | Shutdown::PowerButton | Shutdown::Unexpected
        )
    }
}

/// Windows' own boot measurement, in ms.
#[derive(Clone, Copy, Debug, Default)]
pub struct Measured {
    /// The whole boot, sign-in screen's wait taken out.
    pub total_ms: u64,
    /// To a usable desktop.
    pub main_path_ms: u64,
    /// After the desktop, until the system settled.
    pub after_ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlowKind {
    Program,
    Driver,
    Service,
    Device,
    /// A step of the system's own: policies, the session manager, prefetch.
    System,
}

impl SlowKind {
    pub fn label(self) -> &'static str {
        match self {
            SlowKind::Program => "Program",
            SlowKind::Driver => "Driver",
            SlowKind::Service => "Service",
            SlowKind::Device => "Device",
            SlowKind::System => "System",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Slow {
    pub kind: SlowKind,
    pub name: String,
    /// What it took, in ms.
    pub took_ms: u64,
    /// How much more than usual, in ms; 0 when not known.
    pub over_ms: u64,
}

#[derive(Clone, Debug, Default)]
pub struct Boots {
    /// Newest first.
    pub boots: Vec<Boot>,
    /// What could not be read, in words.
    pub note: Option<String>,
    pub took: Duration,
}

/// Days since 1970-01-01 of a civil date (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// "2026-10-30T13:50:22.5000000Z" (UTC) as a FILETIME.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn filetime_of_iso(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || b[10] != b'T' {
        return None;
    }
    let num = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (y, mo, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (h, mi, se) = (num(11..13)?, num(14..16)?, num(17..19)?);
    let mut ticks = 0i64;
    if b.get(19) == Some(&b'.') {
        let digits: String = s[20..].chars().take_while(char::is_ascii_digit).collect();
        let seven: String = digits
            .chars()
            .chain(std::iter::repeat('0'))
            .take(7)
            .collect();
        ticks = seven.parse().ok()?;
    }
    let unix = days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60 + se;
    Some((unix + 11_644_473_600) * 10_000_000 + ticks)
}

/// A FILETIME from Unix seconds.
#[cfg_attr(windows, allow(dead_code))]
pub(crate) fn filetime_of_unix(secs: f64) -> i64 {
    ((secs + 11_644_473_600.0) * 10_000_000.0) as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_logs_times() {
        // 1970-01-01 is 11644473600 s after 1601-01-01.
        assert_eq!(
            filetime_of_iso("1970-01-01T00:00:00Z"),
            Some(116_444_736_000_000_000)
        );
        let t = filetime_of_iso("2026-10-30T13:50:22.5000000Z").unwrap();
        let u = filetime_of_iso("2026-10-30T13:50:23.0047428Z").unwrap();
        assert_eq!(u - t, 5_047_428);
        assert_eq!(
            (t - 116_444_736_000_000_000) / 10_000_000,
            1_793_368_222,
            "2026-10-30 13:50:22 UTC"
        );
        assert_eq!(filetime_of_iso("not a time"), None);
    }
}

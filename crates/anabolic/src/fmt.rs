//! Number formatting for humans.

use std::fmt::Write;
use std::time::Duration;

pub fn bytes(n: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    if n < 1024 {
        return format!("{n} B");
    }
    let mut value = n as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    let decimals = if value >= 100.0 {
        0
    } else if value >= 10.0 {
        1
    } else {
        2
    };
    format!("{value:.decimals$} {}", UNITS[unit])
}

/// Bytes per second; blank when there is nothing to show.
pub fn rate(bytes_per_sec: f64) -> String {
    if bytes_per_sec < 1.0 {
        String::new()
    } else {
        format!("{}/s", bytes(bytes_per_sec as u64))
    }
}

pub fn rate_always(bytes_per_sec: f64) -> String {
    format!("{}/s", bytes(bytes_per_sec.max(0.0) as u64))
}

/// CPU percentage; blank below the display threshold, "< 0.01" for trickles.
pub fn cpu(pct: f32) -> String {
    if pct <= 0.0 {
        String::new()
    } else if pct < 0.01 {
        "< 0.01".into()
    } else {
        format!("{pct:.2}")
    }
}

pub fn count(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

pub fn duration(d: Duration) -> String {
    let secs = d.as_secs();
    let (days, hours, mins, s) = (secs / 86400, secs / 3600 % 24, secs / 60 % 60, secs % 60);
    let mut out = String::new();
    if days > 0 {
        let _ = write!(out, "{days}d ");
    }
    let _ = write!(out, "{hours:02}:{mins:02}:{s:02}");
    out
}

/// 100 ns units (FILETIME deltas, CPU times) as h:mm:ss.fff.
pub fn cpu_time(hundred_ns: u64) -> String {
    let ms = hundred_ns / 10_000;
    let (h, m, s, ms) = (ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60, ms % 1000);
    format!("{h}:{m:02}:{s:02}.{ms:03}")
}

pub fn millis(d: Duration) -> String {
    format!("{:.1} ms", d.as_secs_f64() * 1000.0)
}

/// An account without its domain, as Task Manager shows it: `SYSTEM`, not
/// `NT AUTHORITY\SYSTEM`. The details pane keeps the whole name.
pub fn account(user: &str) -> &str {
    user.rsplit_once('\\').map_or(user, |(_, name)| name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accounts_lose_their_domain() {
        assert_eq!(account(r"DESKTOP-1\alice"), "alice");
        assert_eq!(account(r"NT AUTHORITY\LOCAL SERVICE"), "LOCAL SERVICE");
        assert_eq!(account("S-1-5-18"), "S-1-5-18");
    }

    #[test]
    fn formats() {
        assert_eq!(bytes(512), "512 B");
        assert_eq!(bytes(1536), "1.50 KB");
        assert_eq!(bytes(150 * 1024 * 1024), "150 MB");
        assert_eq!(count(1234567), "1,234,567");
        assert_eq!(count(999), "999");
        assert_eq!(cpu(0.0), "");
        assert_eq!(cpu(0.004), "< 0.01");
        assert_eq!(cpu(12.345), "12.35");
        assert_eq!(cpu_time(36_610_000_000 + 1_230_000), "1:01:01.123");
        assert_eq!(duration(Duration::from_secs(90061)), "1d 01:01:01");
    }
}

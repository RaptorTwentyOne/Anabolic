// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Small static facts and conversions, from /proc and the C library.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::util::{filetime_from_unix, unix_from_filetime};

/// Marketing name of the first processor, e.g. "13th Gen Intel(R) Core(TM) i9-13980HX".
pub fn cpu_name() -> Option<String> {
    let info = std::fs::read_to_string("/proc/cpuinfo").ok()?;
    info.lines()
        .find_map(|l| {
            let (key, value) = l.split_once(':')?;
            // "model name" on x86, "Model" or "Hardware" elsewhere.
            matches!(key.trim(), "model name" | "Model" | "Hardware").then(|| value.trim())
        })
        .filter(|n| !n.is_empty())
        .map(str::to_string)
}

/// How long the machine has been running, suspended time included.
pub fn uptime() -> Duration {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    if unsafe { libc::clock_gettime(libc::CLOCK_BOOTTIME, &mut ts) } != 0 {
        return Duration::ZERO;
    }
    Duration::new(ts.tv_sec as u64, ts.tv_nsec as u32)
}

pub fn now_filetime() -> i64 {
    filetime_from_unix(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0.0, |d| d.as_secs_f64()),
    )
}

/// A FILETIME (UTC, 100 ns since 1601) as local "YYYY-MM-DD HH:MM:SS".
pub fn local_time(filetime: i64) -> Option<String> {
    if filetime <= 0 {
        return None;
    }
    let secs = unix_from_filetime(filetime).floor() as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&secs, &mut tm) }.is_null() {
        return None;
    }
    Some(format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    ))
}

/// A thread's state, the letter /proc gives it (as `ThreadInfo::state`).
pub fn thread_state_name(state: u32) -> &'static str {
    match char::from_u32(state).unwrap_or('?') {
        'R' => "Running",
        'S' => "Sleeping",
        'D' => "Waiting (uninterruptible)",
        'T' => "Stopped",
        't' => "Stopped (traced)",
        'Z' => "Zombie",
        'X' => "Dead",
        'I' => "Idle",
        'P' => "Parked",
        'W' => "Waking",
        _ => "?",
    }
}

/// What a sleeping thread waits in: the system call it made, numbered
/// from 1 (`ThreadInfo::wait_reason`; 0 when it waits in none).
pub fn wait_reason_name(reason: u32) -> &'static str {
    if reason == 0 {
        return "";
    }
    syscall_name(reason - 1).unwrap_or("?")
}

/// A system call's name by its number (x86-64).
pub fn syscall_name(nr: u32) -> Option<&'static str> {
    crate::syscalls::name(nr)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_name_and_time() {
        assert!(cpu_name().is_some_and(|n| !n.is_empty()));
        let now = local_time(now_filetime()).unwrap();
        assert_eq!(now.len(), 19, "{now}");
        assert!(uptime() > Duration::ZERO);
        assert_eq!(thread_state_name(u32::from(b'S')), "Sleeping");
    }
}

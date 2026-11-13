// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! What changed on this PC, day by day, in the manner of Reliability
//! Monitor and WinUpdatesView: updates installed (and those that failed),
//! drivers installed or updated, programs installed and removed, services
//! and kernel drivers added; beside each day's crashes, so "it started
//! crashing on Tuesday" meets "a driver came on Tuesday". On Windows from
//! the event logs and the programs' own install dates, on Linux from the
//! package manager's log. It only reads.

use std::time::Duration;

#[cfg_attr(windows, path = "windows/changes.rs")]
#[cfg_attr(target_os = "linux", path = "linux/changes.rs")]
mod imp;
pub use imp::read;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    Update,
    UpdateFailed,
    Driver,
    Program,
    ProgramRemoved,
    /// A service or kernel driver set up.
    Service,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Update => "Update",
            Kind::UpdateFailed => "Update failed",
            Kind::Driver => "Driver",
            Kind::Program => "Installed",
            Kind::ProgramRemoved => "Removed",
            Kind::Service => "Service",
        }
    }

    pub const ALL: [Kind; 6] = [
        Kind::Update,
        Kind::UpdateFailed,
        Kind::Driver,
        Kind::Program,
        Kind::ProgramRemoved,
        Kind::Service,
    ];
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    /// As a FILETIME.
    pub at: i64,
    /// Only the day is known (a program's own install date).
    pub day_only: bool,
    pub kind: Kind,
    pub what: String,
    pub detail: String,
}

#[derive(Debug, Default)]
pub struct Changes {
    /// Newest first.
    pub changes: Vec<Change>,
    pub notes: Vec<String>,
    pub took: Duration,
}

/// A FILETIME for midnight (UTC) of a calendar day.
pub fn day_filetime(year: i64, month: u32, day: u32) -> Option<i64> {
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || year < 1970 {
        return None;
    }
    // Days from 1970-01-01, Howard Hinnant's civil-from-days inverse.
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = month as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + day as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some((days * 86_400 + 11_644_473_600) * 10_000_000)
}

/// "20261113" as a FILETIME: noon (UTC), the same date nearly
/// everywhere.
pub fn yyyymmdd(text: &str) -> Option<i64> {
    let t = text.trim();
    if t.len() != 8 || !t.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    day_filetime(
        t[..4].parse().ok()?,
        t[4..6].parse().ok()?,
        t[6..].parse().ok()?,
    )
    .map(|t| t + 12 * 3600 * 10_000_000)
}

/// One calendar day: its changes and how many crashes it saw.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Day {
    /// "2026-11-13", local.
    pub day: String,
    /// Indexes into the changes, newest first.
    pub changes: Vec<usize>,
    pub crashes: usize,
}

fn day_of(filetime: i64) -> String {
    crate::sysinfo::local_time(filetime)
        .map(|t| t.chars().take(10).collect())
        .unwrap_or_default()
}

/// The days, newest first, that saw a change or a crash.
pub fn days(changes: &[Change], crashes: Option<&crate::crashes::Crashes>) -> Vec<Day> {
    fn slot(out: &mut Vec<Day>, day: String) -> &mut Day {
        let i = match out.iter().position(|d| d.day == day) {
            Some(i) => i,
            None => {
                out.push(Day {
                    day,
                    changes: Vec::new(),
                    crashes: 0,
                });
                out.len() - 1
            }
        };
        &mut out[i]
    }
    let mut out: Vec<Day> = Vec::new();
    for (i, c) in changes.iter().enumerate() {
        // A day-only date is noon UTC of the day it names.
        let day = if c.day_only {
            civil((c.at / 10_000_000 - 11_644_473_600).div_euclid(86_400))
        } else {
            day_of(c.at)
        };
        slot(&mut out, day).changes.push(i);
    }
    if let Some(cr) = crashes {
        let first = changes.last().map_or(i64::MAX, |c| c.at);
        for c in cr.crashes.iter().filter(|c| c.at >= first) {
            slot(&mut out, day_of(c.at)).crashes += 1;
        }
    }
    out.sort_by(|a, b| b.day.cmp(&a.day));
    out
}

/// "2026-11-13" from days since 1970-01-01.
fn civil(days: i64) -> String {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!("{y:04}-{m:02}-{d:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_days() {
        let t = yyyymmdd("20261113").unwrap();
        let secs = t / 10_000_000 - 11_644_473_600;
        assert_eq!(civil(secs / 86_400), "2026-11-13");
        assert_eq!(civil(0), "1970-01-01");
        assert!(yyyymmdd("2026-11-13").is_none());
        assert!(yyyymmdd("20261341").is_none());
    }

    #[test]
    fn groups_by_day() {
        let c = |day: &str, kind| Change {
            at: yyyymmdd(day).unwrap(),
            day_only: true,
            kind,
            what: day.into(),
            detail: String::new(),
        };
        let list = [
            c("20261113", Kind::Driver),
            c("20261113", Kind::Program),
            c("20261110", Kind::Update),
        ];
        let d = days(&list, None);
        assert_eq!(d.len(), 2);
        assert_eq!(d[0].day, "2026-11-13");
        assert_eq!(d[0].changes.len(), 2);
    }

    #[test]
    fn reads_this_machines_changes() {
        let c = read().unwrap();
        assert!(!c.changes.is_empty());
        for x in c.changes.iter().take(15) {
            println!(
                "{} {:<14} {} | {}",
                crate::sysinfo::local_time(x.at).unwrap_or_default(),
                x.kind.label(),
                x.what,
                x.detail
            );
        }
        println!("{} changes in {:?}, {:?}", c.changes.len(), c.took, c.notes);
    }
}

// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! What changed on Linux, from the package manager's own log: dpkg's
//! (Debian, Ubuntu) and pacman's (Arch). A kernel or a driver comes as a
//! package too.

use std::time::Instant;

use crate::changes::{Change, Changes, Kind, day_filetime};

/// "2026-11-13 10:12:01" as a FILETIME (UTC; the log's clock is local,
/// close enough for a day).
fn when(date: &str, time: &str) -> Option<i64> {
    let mut d = date.split('-');
    let day = day_filetime(
        d.next()?.parse().ok()?,
        d.next()?.parse().ok()?,
        d.next()?.parse().ok()?,
    )?;
    let mut t = time.split(':');
    let (h, m, s): (i64, i64, i64) = (
        t.next()?.parse().ok()?,
        t.next()?.parse().ok()?,
        t.next()
            .and_then(|x| x.get(..2))
            .unwrap_or("0")
            .parse()
            .ok()?,
    );
    Some(day + (h * 3600 + m * 60 + s) * 10_000_000)
}

fn kind_of(package: &str) -> Kind {
    if package.starts_with("linux-image")
        || package.starts_with("linux") && package.contains("firmware")
    {
        Kind::Driver
    } else {
        Kind::Program
    }
}

fn dpkg(out: &mut Vec<Change>) -> bool {
    let Ok(text) = std::fs::read_to_string("/var/log/dpkg.log") else {
        return false;
    };
    for line in text.lines() {
        // 2026-11-13 10:12:01 install name:amd64 <none> 1.2-3
        let w: Vec<&str> = line.split_whitespace().collect();
        if w.len() < 6 || !matches!(w[2], "install" | "upgrade" | "remove") {
            continue;
        }
        let Some(at) = when(w[0], w[1]) else {
            continue;
        };
        let name = w[3].split(':').next().unwrap_or(w[3]);
        out.push(Change {
            at,
            day_only: false,
            kind: if w[2] == "remove" {
                Kind::ProgramRemoved
            } else {
                kind_of(name)
            },
            what: name.to_string(),
            detail: format!("{} {}", w[2], w[5]),
            times: 1,
        });
    }
    true
}

fn pacman(out: &mut Vec<Change>) -> bool {
    let Ok(text) = std::fs::read_to_string("/var/log/pacman.log") else {
        return false;
    };
    for line in text.lines() {
        // [2026-11-13T10:12:01+0100] [ALPM] upgraded name (1.0 -> 1.1)
        let Some(rest) = line.strip_prefix('[') else {
            continue;
        };
        let Some((stamp, rest)) = rest.split_once(']') else {
            continue;
        };
        let Some(rest) = rest.trim().strip_prefix("[ALPM]") else {
            continue;
        };
        let w: Vec<&str> = rest.split_whitespace().collect();
        if w.len() < 2 || !matches!(w[0], "installed" | "upgraded" | "removed") {
            continue;
        }
        let (date, time) = stamp.split_once('T').unwrap_or((stamp, "00:00:00"));
        let Some(at) = when(date, time) else {
            continue;
        };
        out.push(Change {
            at,
            day_only: false,
            kind: if w[0] == "removed" {
                Kind::ProgramRemoved
            } else {
                kind_of(w[1])
            },
            what: w[1].to_string(),
            detail: format!("{} {}", w[0], w[2..].join(" ")),
            times: 1,
        });
    }
    true
}

pub fn read() -> Result<Changes, String> {
    let started = Instant::now();
    let mut changes = Vec::new();
    let mut notes = Vec::new();
    if !dpkg(&mut changes) && !pacman(&mut changes) {
        notes.push("No package manager log was found (dpkg's or pacman's).".into());
    }
    changes.sort_by_key(|c| std::cmp::Reverse(c.at));
    let mut changes = crate::changes::collapse(changes);
    changes.truncate(5000);
    Ok(Changes {
        changes,
        notes,
        took: started.elapsed(),
    })
}

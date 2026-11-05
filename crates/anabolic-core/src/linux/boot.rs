// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Boots from systemd: this boot's timings and its slowest units from
//! `systemd-analyze`, the earlier boots' times from the journal.

use std::process::Command;
use std::time::Instant;

use crate::boot::{Boot, BootKind, Boots, Slow, SlowKind, filetime_of_unix};

const SLOWEST: usize = 15;

fn run(program: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(program).args(args).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// "1min 2.345s", "345ms", "4.5s", "1h 2min" in ms.
fn span(text: &str) -> Option<u64> {
    let mut total = 0f64;
    let mut any = false;
    for token in text.split_whitespace() {
        let split = token
            .find(|c: char| !(c.is_ascii_digit() || c == '.'))
            .unwrap_or(token.len());
        let (value, unit) = token.split_at(split);
        let value: f64 = value.parse().ok()?;
        total += value
            * match unit {
                "us" | "µs" => 0.001,
                "ms" => 1.0,
                "s" => 1000.0,
                "min" => 60_000.0,
                "h" => 3_600_000.0,
                _ => return None,
            };
        any = true;
    }
    any.then_some(total.round() as u64)
}

/// "Startup finished in 3.1s (firmware) + 2.4s (loader) + 1.4s (kernel) +
/// 5.3s (initrd) + 14.2s (userspace) = 26.6s": (firmware and loader, the
/// rest), in ms.
fn startup(text: &str) -> (Option<u64>, Option<u64>) {
    let Some(line) = text.lines().find(|l| l.starts_with("Startup finished in ")) else {
        return (None, None);
    };
    let parts = line["Startup finished in ".len()..]
        .split(" = ")
        .next()
        .unwrap_or("");
    let (mut before, mut after) = (None::<u64>, None::<u64>);
    for part in parts.split(" + ") {
        let Some((time, what)) = part.trim().split_once(" (") else {
            continue;
        };
        let Some(ms) = span(time) else { continue };
        let slot = if what.starts_with("firmware") || what.starts_with("loader") {
            &mut before
        } else {
            &mut after
        };
        *slot = Some(slot.unwrap_or(0) + ms);
    }
    (before, after)
}

/// "14.211s NetworkManager-wait-online.service" lines.
fn blame(text: &str) -> Vec<Slow> {
    text.lines()
        .filter_map(|l| {
            let l = l.trim();
            // The time is all but the last word: "1min 2.3s unit".
            let (time, unit) = l.rsplit_once(' ')?;
            Some(Slow {
                kind: SlowKind::Service,
                name: unit.to_string(),
                took_ms: span(time)?,
                over_ms: 0,
            })
        })
        .take(SLOWEST)
        .collect()
}

pub fn read() -> Result<Boots, String> {
    let started = Instant::now();
    let uptime: f64 = std::fs::read_to_string("/proc/uptime")
        .ok()
        .and_then(|s| s.split_whitespace().next()?.parse().ok())
        .ok_or("/proc/uptime could not be read")?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    let mut note = None;
    let (firmware_ms, ready_ms) = match run("systemd-analyze", &["time"]) {
        Some(t) => startup(&t),
        None => {
            note = Some("systemd-analyze gave no timings (not systemd, or still starting)".into());
            (None, None)
        }
    };
    let slowed = run("systemd-analyze", &["blame", "--no-pager"])
        .map(|t| blame(&t))
        .unwrap_or_default();
    let mut boots = vec![Boot {
        started: filetime_of_unix(now - uptime),
        kind: BootKind::Cold,
        firmware_ms,
        ready_ms,
        slowed,
        ..Boot::default()
    }];
    // The earlier boots the journal keeps, by when they began.
    if let Some(list) = run(
        "journalctl",
        &["--list-boots", "-o", "json", "--no-pager", "-q"],
    ) && let Ok(serde_json::Value::Array(items)) = serde_json::from_str(&list)
    {
        let mut earlier: Vec<i64> = items
            .iter()
            .filter(|b| b["index"].as_i64().is_some_and(|i| i < 0))
            .filter_map(|b| b["first_entry"].as_u64())
            .map(|us| filetime_of_unix(us as f64 / 1e6))
            .collect();
        earlier.sort_unstable_by(|a, b| b.cmp(a));
        boots.extend(earlier.into_iter().map(|started| Boot {
            started,
            ..Boot::default()
        }));
    }
    Ok(Boots {
        boots,
        note,
        took: started.elapsed(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_systemd_analyze() {
        assert_eq!(span("1min 2.5s"), Some(62_500));
        assert_eq!(span("345ms"), Some(345));
        assert_eq!(span("1h 2min"), Some(3_720_000));
        assert_eq!(span("nonsense"), None);
        let (before, after) = startup(
            "Startup finished in 3.155s (firmware) + 2.476s (loader) + 1.468s (kernel) + \
             5.380s (initrd) + 14.211s (userspace) = 26.692s\n\
             graphical.target reached after 14.1s in userspace.",
        );
        assert_eq!(before, Some(5631));
        assert_eq!(after, Some(21_059));
        let slow = blame("14.211s NetworkManager-wait-online.service\n1min 2.3s big.service\n");
        assert_eq!(slow.len(), 2);
        assert_eq!(slow[1].name, "big.service");
        assert_eq!(slow[1].took_ms, 62_300);
    }
}

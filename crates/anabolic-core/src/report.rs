// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! A report of this PC to paste into a forum post or a support ticket, in
//! the manner of Speccy: the system, processor, memory, graphics,
//! monitors, drives with their health, network adapters, what the checkup
//! found, devices with a problem and the last week's crashes, in Markdown.
//! Names, serial numbers and addresses are left out: no computer or user
//! name, no module or disk serial, no MAC or IP address, no paths.

use std::fmt::Write;

use crate::Snapshot;
use crate::checkup::{Checkup, Verdict};
use crate::crashes::Crashes;
use crate::devices::Devices;
use crate::displays::Display;
use crate::health::Health;
use crate::hwinfo::HardwareInfo;

#[cfg_attr(windows, path = "windows/report.rs")]
#[cfg_attr(target_os = "linux", path = "linux/report.rs")]
mod imp;
pub use imp::os_version;

/// What a report is made of; parts not read yet are left out.
pub struct Parts<'a> {
    pub snap: &'a Snapshot,
    pub hw: &'a HardwareInfo,
    pub displays: Option<&'a [Display]>,
    pub health: &'a [Health],
    pub devices: Option<&'a Devices>,
    pub crashes: Option<&'a Crashes>,
    pub checkup: Option<&'a Checkup>,
}

fn bytes(b: u64) -> String {
    const GB: f64 = (1u64 << 30) as f64;
    let g = b as f64 / GB;
    if g >= 1000.0 {
        format!("{:.2} TB", g / 1024.0)
    } else if g >= 10.0 {
        format!("{g:.0} GB")
    } else if g >= 1.0 {
        format!("{g:.1} GB")
    } else {
        format!("{} MB", b >> 20)
    }
}

fn span(secs: u64) -> String {
    let (d, h, m) = (secs / 86_400, secs / 3600 % 24, secs / 60 % 60);
    match (d, h) {
        (0, 0) => format!("{m} min"),
        (0, _) => format!("{h} h {m} min"),
        _ => format!("{d} d {h} h"),
    }
}

/// Drops what could tell who someone is: a path to a home folder.
fn plain(text: &str) -> String {
    text.split_whitespace()
        .filter(|w| !w.contains(":\\Users\\") && !w.starts_with("/home/"))
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn build(p: &Parts<'_>) -> String {
    let mut out = String::new();
    let s = &p.snap.system;
    let hw = p.hw;
    let _ = writeln!(out, "# PC report\n");
    let _ = writeln!(
        out,
        "Made by Anabolic {} on {}. Names, serial numbers and addresses are left out.\n",
        env!("CARGO_PKG_VERSION"),
        crate::sysinfo::local_time(crate::sysinfo::now_filetime())
            .map(|t| t.chars().take(16).collect::<String>())
            .unwrap_or_default()
    );

    let _ = writeln!(out, "## System");
    if let Some(os) = os_version() {
        let _ = writeln!(out, "- {os}");
    }
    let b = &hw.board;
    if !b.product.is_empty() {
        let _ = writeln!(
            out,
            "- Board: {} {}, firmware {} ({})",
            b.manufacturer.trim(),
            b.product.trim(),
            b.bios_version.trim(),
            b.bios_date.trim()
        );
    }
    let _ = writeln!(out, "- Up for {}", span(crate::sysinfo::uptime().as_secs()));

    let c = &hw.cpu;
    let _ = writeln!(out, "\n## Processor");
    let _ = writeln!(
        out,
        "- {}: {} cores, {} threads, {:.2} to {:.2} GHz{}",
        c.brand.trim(),
        c.cores,
        c.threads,
        c.base_mhz as f32 / 1000.0,
        c.max_mhz as f32 / 1000.0,
        c.microcode
            .as_ref()
            .map_or(String::new(), |m| format!(", microcode {m}"))
    );
    let _ = writeln!(out, "- Busy now: {:.0} %", s.cpu.total);

    let _ = writeln!(out, "\n## Memory");
    let m = &hw.memory;
    let mods = &m.modules;
    if let Some(first) = mods.first() {
        let running = first.configured_speed.max(first.speed);
        let rated = mods
            .iter()
            .map(|x| {
                crate::checkup::rated_speed(&x.part_number)
                    .unwrap_or(0)
                    .max(x.speed)
            })
            .max()
            .unwrap_or(0);
        let mut parts: Vec<&str> = mods.iter().map(|x| x.part_number.trim()).collect();
        parts.sort();
        parts.dedup();
        let _ = writeln!(
            out,
            "- {} {} in {} of {} slots, {} MT/s (made for {} MT/s), {}",
            bytes(if m.installed > 0 {
                m.installed
            } else {
                mods.iter().map(|x| x.size).sum()
            }),
            first.kind,
            mods.len(),
            m.slots.max(mods.len()),
            running,
            rated,
            parts.join(", ")
        );
    } else {
        let _ = writeln!(out, "- {}", bytes(s.memory.total));
    }
    let _ = writeln!(
        out,
        "- In use {} ({:.0} %), committed {} of {}",
        bytes(s.memory.used()),
        s.memory.used() as f64 * 100.0 / s.memory.total.max(1) as f64,
        bytes(s.memory.commit),
        bytes(s.memory.commit_limit)
    );

    let _ = writeln!(out, "\n## Graphics");
    for g in &s.gpus {
        let driver = p.devices.and_then(|d| {
            d.devices
                .iter()
                .find(|x| x.name == *g.name)
                .and_then(|x| x.driver.as_ref())
                .and_then(|x| x.version.clone())
        });
        let _ = writeln!(
            out,
            "- {}{}{}",
            g.name,
            if g.dedicated_total > 0 {
                format!(", {}", bytes(g.dedicated_total))
            } else {
                String::new()
            },
            driver.map_or(String::new(), |v| format!(", driver {v}"))
        );
    }
    if let Some(displays) = p.displays {
        for d in displays {
            let _ = writeln!(
                out,
                "- Monitor {}: {}×{} at {:.0} Hz (best {} Hz, panel {}×{}){}",
                if d.name.is_empty() { "?" } else { &d.name },
                d.width,
                d.height,
                d.refresh_hz,
                d.max_hz_here,
                d.native.0,
                d.native.1,
                if d.hdr_on { ", HDR on" } else { "" }
            );
        }
    }

    let _ = writeln!(out, "\n## Drives");
    for d in &s.disks {
        let h = p.health.iter().find(|h| h.number == d.number);
        let mut line = format!(
            "- Disk {}: {}, {} {}, {}",
            d.number,
            d.model.trim(),
            d.bus,
            match d.ssd {
                Some(true) => "SSD",
                Some(false) => "hard disk",
                None => "drive",
            },
            bytes(d.size)
        );
        if let Some(h) = h {
            let _ = write!(line, ", health {}", h.verdict.label());
            if let Some(w) = h.wear_used {
                let _ = write!(line, ", {w} % worn");
            }
            if let Some(t) = h.temperature {
                let _ = write!(line, ", {t} °C");
            }
            if !h.reasons.is_empty() {
                let _ = write!(line, " ({})", h.reasons.join("; "));
            }
        }
        let _ = writeln!(out, "{line}");
        for v in &d.volumes {
            let _ = writeln!(
                out,
                "  - {} {} free of {}",
                v.letter,
                bytes(v.free),
                bytes(v.total)
            );
        }
    }

    let _ = writeln!(out, "\n## Network");
    for a in &s.net.adapters {
        let _ = writeln!(
            out,
            "- {}: {}",
            a.description,
            if a.link_speed >= 1_000_000_000 {
                format!("{} Gbps", a.link_speed as f64 / 1e9)
            } else {
                format!("{} Mbps", a.link_speed / 1_000_000)
            }
        );
    }

    if let Some(c) = p.checkup {
        let asks: Vec<_> = c
            .checks
            .iter()
            .filter(|x| matches!(x.verdict, Verdict::Change | Verdict::Consider))
            .collect();
        let _ = writeln!(out, "\n## Checkup");
        if asks.is_empty() {
            let _ = writeln!(out, "- Nothing that costs speed");
        }
        for x in asks {
            let _ = writeln!(out, "- {}: {}: {}", x.verdict.label(), x.title, x.now);
        }
    }

    if let Some(d) = p.devices {
        let bad = crate::devices::troubled(&d.devices);
        if !bad.is_empty() {
            let _ = writeln!(out, "\n## Devices with a problem");
            for x in bad {
                let _ = writeln!(out, "- {}: {} ({})", x.name, x.state.label(), x.class);
            }
        }
    }

    if let Some(c) = p.crashes {
        let week = crate::sysinfo::now_filetime() - 7 * 86_400 * 10_000_000;
        let recent: Vec<crate::crashes::Crash> =
            c.crashes.iter().filter(|x| x.at >= week).cloned().collect();
        let groups = crate::crashes::groups(&recent);
        let _ = writeln!(out, "\n## Crashes in the last 7 days");
        if groups.is_empty() {
            let _ = writeln!(out, "- None");
        }
        for g in groups.iter().take(12) {
            let _ = writeln!(
                out,
                "- {}: {} time{}{}",
                g.program,
                g.items.len(),
                if g.items.len() == 1 { "" } else { "s" },
                g.module
                    .as_ref()
                    .map_or(String::new(), |(m, n)| format!(", {n} in {}", plain(m)))
            );
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leaves_out_home_folders() {
        assert_eq!(plain(r"C:\Users\alice\app.dll"), "");
        assert_eq!(plain("ntdll.dll"), "ntdll.dll");
        assert_eq!(span(3 * 3600 + 120), "3 h 2 min");
        assert_eq!(bytes(137_438_953_472), "128 GB");
    }

    #[test]
    fn says_which_windows() {
        let os = os_version().unwrap();
        println!("{os}");
        assert!(!os.is_empty());
    }
}

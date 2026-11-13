// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! The checkup's Linux part: PCIe links from sysfs, the processor's
//! frequency policy and the firmware's platform profile, a restart the
//! packages ask for, and TRIM. Nothing is changed.

use std::path::Path;
use std::process::Command;

use crate::checkup::{Area, Check, Fix, Link, Slot, Verdict, integrated, link};

fn read(path: impl AsRef<Path>) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// "16.0 GT/s PCIe" as a generation.
fn generation(speed: &str) -> u32 {
    let gts: f32 = speed
        .split_whitespace()
        .next()
        .and_then(|n| n.parse().ok())
        .unwrap_or(0.0);
    match gts {
        x if x >= 60.0 => 6,
        x if x >= 30.0 => 5,
        x if x >= 15.0 => 4,
        x if x >= 7.0 => 3,
        x if x >= 4.0 => 2,
        x if x > 0.0 => 1,
        _ => 0,
    }
}

pub fn checks() -> Vec<Check> {
    let mut out = pci();
    out.extend(cpu_policy());
    out.push(restart());
    out.extend(trim());
    out
}

fn pci() -> Vec<Check> {
    let mut out = Vec::new();
    let names: Vec<(String, String)> = crate::devices::read()
        .map(|d| d.devices.into_iter().map(|x| (x.id, x.name)).collect())
        .unwrap_or_default();
    let Ok(dir) = std::fs::read_dir("/sys/bus/pci/devices") else {
        return out;
    };
    let mut paths: Vec<_> = dir.flatten().map(|e| e.path()).collect();
    paths.sort();
    let (mut gpus, mut drives) = (0, 0);
    for p in paths {
        let class = read(p.join("class")).unwrap_or_default();
        let gpu = class.starts_with("0x0300") || class.starts_with("0x0302");
        let nvme = class.starts_with("0x010802");
        if !gpu && !nvme {
            continue;
        }
        let number = |f: &str| read(p.join(f)).and_then(|v| v.parse().ok()).unwrap_or(0);
        let l = Link {
            width: number("current_link_width"),
            max_width: number("max_link_width"),
            generation: generation(&read(p.join("current_link_speed")).unwrap_or_default()),
            max_generation: generation(&read(p.join("max_link_speed")).unwrap_or_default()),
        };
        let slot = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let name = names
            .iter()
            .find(|(id, _)| id.ends_with(slot))
            .map(|(_, n)| n.clone())
            .unwrap_or_else(|| slot.to_string());
        if gpu {
            if integrated(&name) {
                continue;
            }
            out.extend(link(Slot::Graphics, gpus, &name, l));
            gpus += 1;
        } else {
            out.extend(link(Slot::Drive, drives, &name, l));
            drives += 1;
        }
    }
    out
}

fn cpu_policy() -> Vec<Check> {
    let mut out = Vec::new();
    let base = "/sys/devices/system/cpu/cpu0/cpufreq";
    let driver = read(format!("{base}/scaling_driver")).unwrap_or_default();
    let governor = read(format!("{base}/scaling_governor")).unwrap_or_default();
    let epp = read(format!("{base}/energy_performance_preference"));
    // With amd-pstate and intel_pstate in active mode, "powersave" is the
    // usual governor: the energy preference decides.
    let active = driver == "amd-pstate-epp" || driver == "intel_pstate";
    if let Some(epp) = epp.filter(|_| active) {
        out.push(if epp == "power" {
            Check::new(
                "cpu-energy",
                Area::Power,
                "Processor energy preference",
                Verdict::Consider,
                format!("{epp} ({driver})"),
                "The processor favours saving energy: it boosts later and less.",
            )
            .fix(Fix::Hand(
                "The desktop's power profile (power-profiles-daemon, TuneD) sets it.".into(),
            ))
        } else {
            Check::new(
                "cpu-energy",
                Area::Power,
                "Processor energy preference",
                Verdict::Good,
                format!("{epp} ({driver})"),
                "",
            )
        });
    } else if !governor.is_empty() {
        out.push(if governor == "powersave" {
            Check::new(
                "cpu-governor",
                Area::Power,
                "Frequency governor",
                Verdict::Consider,
                format!("{governor} ({driver})"),
                "The processor is held to its lowest clocks.",
            )
        } else {
            Check::new(
                "cpu-governor",
                Area::Power,
                "Frequency governor",
                Verdict::Good,
                format!("{governor} ({driver})"),
                "",
            )
        });
    }
    if let Some(profile) = read("/sys/firmware/acpi/platform_profile") {
        out.push(Check::new(
            "platform-profile",
            Area::Power,
            "Platform profile",
            if profile == "low-power" || profile == "quiet" {
                Verdict::Consider
            } else {
                Verdict::Good
            },
            profile,
            "",
        ));
    }
    out
}

fn restart() -> Check {
    if Path::new("/var/run/reboot-required").exists() {
        Check::new(
            "restart-pending",
            Area::System,
            "Restart",
            Verdict::Consider,
            "Updates wait for a restart",
            "An updated kernel or library takes effect at the next restart.",
        )
    } else {
        Check::new(
            "restart-pending",
            Area::System,
            "Restart",
            Verdict::Good,
            "No restart asked for",
            "",
        )
    }
}

/// SSDs are trimmed by fstrim.timer or by the discard mount option.
fn trim() -> Option<Check> {
    let ssd = std::fs::read_dir("/sys/block").ok()?.flatten().any(|e| {
        let name = e.file_name().to_string_lossy().into_owned();
        !name.starts_with("loop")
            && !name.starts_with("zram")
            && read(e.path().join("queue/rotational")).as_deref() == Some("0")
    });
    if !ssd {
        return None;
    }
    let timer = Command::new("systemctl")
        .args(["is-enabled", "fstrim.timer"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "enabled")
        .unwrap_or(false);
    let discard = read("/proc/mounts").is_some_and(|m| m.contains("discard"));
    Some(if timer || discard {
        Check::new(
            "trim",
            Area::Storage,
            "TRIM",
            Verdict::Good,
            if timer {
                "fstrim.timer is on"
            } else {
                "The discard mount option"
            },
            "",
        )
    } else {
        Check::new(
            "trim",
            Area::Storage,
            "TRIM",
            Verdict::Consider,
            "Neither fstrim.timer nor discard",
            "SSDs are not told which blocks are free: writing slows down over time.",
        )
        .fix(Fix::Hand("sudo systemctl enable --now fstrim.timer".into()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_link_speeds() {
        assert_eq!(generation("16.0 GT/s PCIe"), 4);
        assert_eq!(generation("2.5 GT/s PCIe"), 1);
        assert_eq!(generation("32.0 GT/s PCIe"), 5);
        assert_eq!(generation("Unknown"), 0);
    }
}

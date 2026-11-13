// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Wi-Fi on Linux through NetworkManager's nmcli, which lists the access
//! points it heard without root (a scan of one's own needs it).

use std::process::Command;

use crate::wifi::{AccessPoint, Link, Wifi, channel};

/// nmcli's terse output escapes ':' in values with a backslash.
fn fields(line: &str) -> Vec<String> {
    let mut out = vec![String::new()];
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                if let Some(n) = chars.next() {
                    out.last_mut().unwrap().push(n);
                }
            }
            ':' => out.push(String::new()),
            c => out.last_mut().unwrap().push(c),
        }
    }
    out
}

pub fn read() -> Result<Wifi, String> {
    let run = Command::new("nmcli")
        .args([
            "-t",
            "-f",
            "IN-USE,SSID,BSSID,FREQ,SIGNAL,SECURITY,RATE",
            "device",
            "wifi",
            "list",
        ])
        .output()
        .map_err(|_| "nmcli (NetworkManager) is not there to list the networks".to_string())?;
    if !run.status.success() {
        return Err(String::from_utf8_lossy(&run.stderr).trim().to_string());
    }
    let mut out = Wifi {
        adapter: "Wi-Fi".into(),
        ..Wifi::default()
    };
    for line in String::from_utf8_lossy(&run.stdout).lines() {
        let f = fields(line);
        if f.len() < 7 {
            continue;
        }
        let mhz: u32 = f[3]
            .split_whitespace()
            .next()
            .and_then(|n| n.parse().ok())
            .unwrap_or(0);
        let quality: u32 = f[4].parse().unwrap_or(0);
        let found = channel(mhz);
        let point = AccessPoint {
            ssid: f[1].clone(),
            bssid: f[2].to_uppercase(),
            band: found.map(|c| c.0),
            channel: found.map_or(0, |c| c.1),
            // nmcli gives a quality, not dBm: the usual mapping back.
            dbm: (quality as i32 / 2) - 100,
            quality,
            generation: String::new(),
            security: if f[5].is_empty() || f[5] == "--" {
                "Open".into()
            } else {
                f[5].clone()
            },
            connected: f[0] == "*",
        };
        if point.connected {
            out.link = Some(Link {
                ssid: point.ssid.clone(),
                bssid: point.bssid.clone(),
                band: point.band,
                channel: point.channel,
                quality,
                rx_mbps: f[6]
                    .split_whitespace()
                    .next()
                    .and_then(|n| n.parse().ok())
                    .unwrap_or(0),
                tx_mbps: 0,
                generation: String::new(),
            });
        }
        out.points.push(point);
    }
    out.points.sort_by_key(|p| std::cmp::Reverse(p.dbm));
    Ok(out)
}

#[cfg(test)]
mod tests {
    #[test]
    fn splits_terse_lines() {
        assert_eq!(
            super::fields(r"*:Home:AA\:BB\:CC\:DD\:EE\:FF:2437 MHz:80:WPA2:270 Mbit/s"),
            [
                "*",
                "Home",
                "AA:BB:CC:DD:EE:FF",
                "2437 MHz",
                "80",
                "WPA2",
                "270 Mbit/s"
            ]
        );
    }
}

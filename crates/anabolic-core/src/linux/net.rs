// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Network throughput per adapter from /sys/class/net, and what each one
//! is from udev's hardware database.

use std::collections::HashMap;

use super::*;

/// A sysfs value of one interface.
fn read(name: &str, file: &str) -> Option<String> {
    std::fs::read_to_string(format!("/sys/class/net/{name}/{file}"))
        .ok()
        .map(|s| s.trim().to_string())
}

/// What udev knows of the interface: its model, else its driver.
fn description(index: u64, wireless: bool) -> String {
    let data = std::fs::read_to_string(format!("/run/udev/data/n{index}")).unwrap_or_default();
    let value = |key: &str| {
        data.lines()
            .find_map(|l| l.strip_prefix("E:")?.strip_prefix(key)?.strip_prefix('='))
            .map(str::to_string)
    };
    value("ID_MODEL_FROM_DATABASE")
        .or_else(|| value("ID_NET_DRIVER"))
        .unwrap_or_else(|| if wireless { "Wi-Fi" } else { "Ethernet" }.into())
}

impl NetSampler {
    /// Counts only hardware adapters that are up: bridges, containers'
    /// virtual pairs, VPN tunnels and loopback would count the same bytes
    /// twice.
    pub fn sample(&mut self, secs: f64) -> NetStats {
        let mut stats = NetStats::default();
        let Ok(dir) = std::fs::read_dir("/sys/class/net") else {
            return stats;
        };
        let mut next = HashMap::with_capacity(self.prev.len());
        let mut names: Vec<String> = dir
            .filter_map(|e| e.ok()?.file_name().into_string().ok())
            .collect();
        names.sort();
        for name in names {
            let base = format!("/sys/class/net/{name}");
            let hardware = std::path::Path::new(&format!("{base}/device")).exists();
            if !hardware || read(&name, "operstate").as_deref() != Some("up") {
                continue;
            }
            let Some(luid) = read(&name, "ifindex").and_then(|i| i.parse::<u64>().ok()) else {
                continue;
            };
            let counter = |file: &str| {
                read(&name, &format!("statistics/{file}"))
                    .and_then(|v| v.parse::<u64>().ok())
                    .unwrap_or(0)
            };
            let (rx_total, tx_total) = (counter("rx_bytes"), counter("tx_bytes"));
            let (rx_rate, tx_rate) = match self.prev.get(&luid) {
                Some(&(rx, tx)) => (
                    rx_total.saturating_sub(rx) as f64 / secs,
                    tx_total.saturating_sub(tx) as f64 / secs,
                ),
                None => (0.0, 0.0),
            };
            next.insert(luid, (rx_total, tx_total));
            stats.rx_rate += rx_rate;
            stats.tx_rate += tx_rate;
            let wireless = std::path::Path::new(&format!("{base}/wireless")).exists();
            // Megabits per second; Wi-Fi does not say here.
            let link_speed = read(&name, "speed")
                .and_then(|s| s.parse::<i64>().ok())
                .filter(|&s| s > 0)
                .map_or(0, |mbps| mbps as u64 * 1_000_000);
            stats.adapters.push(AdapterStats {
                luid,
                description: description(luid, wireless),
                name,
                link_speed,
                rx_rate,
                tx_rate,
                rx_total,
                tx_total,
            });
        }
        self.prev = next;
        stats
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_quickly() {
        let mut sampler = NetSampler::default();
        let started = std::time::Instant::now();
        let first = sampler.sample(1.0);
        assert!(started.elapsed().as_millis() < 200);
        assert!(
            first
                .adapters
                .iter()
                .all(|a| !a.name.is_empty() && a.luid > 0)
        );
    }
}

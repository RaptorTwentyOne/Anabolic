// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! The processor layout from /sys/devices/system/cpu: cores, the caches
//! they share, NUMA nodes, and on hybrid Intel processors which cores are
//! performance and which efficiency cores.

use std::collections::HashMap;

use super::*;

fn read(path: &str) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
}

/// "0-3,8,10-11" as the processors it names.
pub(crate) fn parse_list(list: &str) -> Vec<usize> {
    list.split(',')
        .filter(|p| !p.is_empty())
        .flat_map(|part| {
            let (a, b) = part.split_once('-').unwrap_or((part, part));
            let (a, b) = (a.trim().parse().unwrap_or(0), b.trim().parse().unwrap_or(0));
            a..=b
        })
        .collect()
}

/// "48K", "2048K", "36M" in bytes.
fn parse_size(size: &str) -> u32 {
    let (number, unit) = size.split_at(
        size.find(|c: char| !c.is_ascii_digit())
            .unwrap_or(size.len()),
    );
    let n: u32 = number.parse().unwrap_or(0);
    match unit {
        "K" => n * 1024,
        "M" => n * 1024 * 1024,
        _ => n,
    }
}

pub fn topology() -> Topology {
    let online = read("/sys/devices/system/cpu/online").map_or_else(Vec::new, |l| parse_list(&l));
    let total = online.iter().max().map_or(0, |&m| m + 1);
    let mut topo = Topology {
        cpus: vec![LogicalCpu::default(); total],
        ..Default::default()
    };
    // Hybrid Intel processors list their performance cores here.
    let performance = read("/sys/devices/cpu_core/cpus").map(|l| parse_list(&l));
    let mut cores: HashMap<(u32, u32, u32), usize> = HashMap::new();
    let mut caches: HashMap<(u8, String), usize> = HashMap::new();
    for &cpu in &online {
        let base = format!("/sys/devices/system/cpu/cpu{cpu}");
        let id = |file: &str| {
            read(&format!("{base}/topology/{file}"))
                .and_then(|v| v.parse::<u32>().ok())
                .unwrap_or(0)
        };
        let key = (id("physical_package_id"), id("die_id"), id("core_id"));
        let next = cores.len();
        let core = *cores.entry(key).or_insert(next);
        let siblings = read(&format!("{base}/topology/thread_siblings_list"))
            .map_or_else(Vec::new, |l| parse_list(&l));
        let numa = std::fs::read_dir(&base)
            .ok()
            .and_then(|d| {
                d.flatten().find_map(|e| {
                    e.file_name()
                        .to_str()?
                        .strip_prefix("node")?
                        .parse::<u32>()
                        .ok()
                })
            })
            .unwrap_or(0);
        let c = &mut topo.cpus[cpu];
        c.core = core;
        c.siblings = siblings.into_iter().filter(|&s| s != cpu).collect();
        c.numa = numa;
        c.efficiency_class = match &performance {
            Some(p) if p.contains(&cpu) => 1,
            _ => 0,
        };
        for index in 0.. {
            let dir = format!("{base}/cache/index{index}");
            let Some(level) = read(&format!("{dir}/level")).and_then(|l| l.parse::<u8>().ok())
            else {
                break;
            };
            // Unified or data caches only.
            if read(&format!("{dir}/type")).as_deref() == Some("Instruction") {
                continue;
            }
            let shared = read(&format!("{dir}/shared_cpu_list")).unwrap_or_default();
            let next = topo.caches.len();
            let at = *caches.entry((level, shared.clone())).or_insert_with(|| {
                topo.caches.push(Cache {
                    level,
                    size: read(&format!("{dir}/size")).map_or(0, |s| parse_size(&s)),
                    cpus: parse_list(&shared),
                });
                next
            });
            let c = &mut topo.cpus[cpu];
            match level {
                1 => c.l1d = Some(at),
                2 => c.l2 = Some(at),
                3 => c.l3 = Some(at),
                _ => {}
            }
        }
    }
    topo.cores = cores.len();
    topo
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_lists_and_sizes() {
        assert_eq!(parse_list("0-3,8,10-11"), vec![0, 1, 2, 3, 8, 10, 11]);
        assert_eq!(parse_size("48K"), 48 * 1024);
        assert_eq!(parse_size("36M"), 36 * 1024 * 1024);
    }

    #[test]
    fn every_processor_has_a_core_and_caches() {
        let t = topology();
        let n = std::thread::available_parallelism().map_or(1, |n| n.get());
        assert_eq!(t.cpus.len(), n);
        assert!(t.cores > 0 && t.cores <= n);
        assert!(t.cpus.iter().all(|c| c.l1d.is_some() && c.l2.is_some()));
    }
}

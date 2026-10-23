// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Handle counts by object type for a PID, read from the system handle table
//! (the target process is not opened). For process handles it also works out
//! how many distinct processes are referenced and how many of those exited.
use std::collections::{HashMap, HashSet};

use anabolic_core::handles::{count_by_type, object_types, process_handles};
use anabolic_core::hardening::enable_privilege;
use anabolic_core::scan::ProcessScanner;
use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
use windows::core::w;

pub fn main() {
    let pid: u32 = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .expect("usage: handle_summary <pid>");
    let _ = enable_privilege(w!("SeDebugPrivilege"));
    let types = object_types();
    let names = types.iter().map(|(i, t)| (*i, t.name.clone())).collect();

    // Hold a handle to every live process so their kernel objects show up in our own handle table.
    let mut scanner = ProcessScanner::new();
    scanner.refresh().unwrap();
    let live: Vec<(u32, windows::Win32::Foundation::HANDLE)> = scanner
        .entries()
        .filter(|e| e.pid() > 4)
        .filter_map(|e| {
            Some((
                e.pid(),
                unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, e.pid()) }.ok()?,
            ))
        })
        .collect();

    let t = std::time::Instant::now();
    let (entries, total) = process_handles(pid).expect("handle query failed");
    let (own, _) = process_handles(std::process::id()).unwrap();
    println!(
        "pid {pid}: {} handles ({} system-wide), took {:?}",
        entries.len(),
        total,
        t.elapsed()
    );
    for tc in count_by_type(&entries, &names).iter().take(8) {
        println!(
            "{:>10}  {:5.1}%  {}",
            tc.count,
            tc.count as f64 * 100.0 / entries.len().max(1) as f64,
            tc.name
        );
    }

    let process_type = types
        .iter()
        .find(|(_, t)| &*t.name == "Process")
        .map(|(i, _)| *i)
        .unwrap();
    let by_handle: HashMap<u64, u32> = live.iter().map(|(p, h)| (h.0 as u64, *p)).collect();
    let live_objects: HashMap<u64, u32> = own
        .iter()
        .filter_map(|e| Some((e.object, *by_handle.get(&e.value)?)))
        .collect();
    let targets: Vec<u64> = entries
        .iter()
        .filter(|e| e.type_index == process_type)
        .map(|e| e.object)
        .collect();
    let distinct: HashSet<u64> = targets.iter().copied().collect();
    let distinct_live = distinct
        .iter()
        .filter(|o| live_objects.contains_key(o))
        .count();
    let handles_to_dead = targets
        .iter()
        .filter(|o| !live_objects.contains_key(o))
        .count();
    println!();
    println!(
        "process objects system-wide: {}   live processes: {}",
        types[&process_type].objects,
        scanner.entries().count()
    );
    println!(
        "object addresses visible: {}",
        targets.iter().any(|&o| o != 0)
    );
    println!(
        "{} process handles -> {} distinct processes: {} still running, {} exited (zombies kept alive)",
        targets.len(),
        distinct.len(),
        distinct_live,
        distinct.len() - distinct_live
    );
    println!("handles pointing at exited processes: {handles_to_dead}");
    let mut per_live: HashMap<u32, usize> = HashMap::new();
    for o in &targets {
        if let Some(p) = live_objects.get(o) {
            *per_live.entry(*p).or_default() += 1;
        }
    }
    let mut top: Vec<_> = per_live.into_iter().collect();
    top.sort_by_key(|x| std::cmp::Reverse(x.1));
    let names_by_pid: HashMap<u32, String> = scanner
        .entries()
        .map(|e| {
            (
                e.pid(),
                String::from_utf16_lossy(&e.image_name())
                    .rsplit(std::path::MAIN_SEPARATOR)
                    .next()
                    .unwrap_or("")
                    .to_string(),
            )
        })
        .collect();
    for (p, n) in top.iter().take(8) {
        println!(
            "{n:>8} handles -> PID {p} {}",
            names_by_pid.get(p).map_or("", |s| s)
        );
    }
}

//! Records what a process does, Process Monitor style, and prints the
//! timeline. Needs an elevated shell.
//!
//!   record <pid> [secs]                    a running process (and children)
//!   record --all [secs]                    every process (counts only)
//!   record --launch <program> [args...]    start one and record it
//!   record --hidden <program> [args...]    the same, no window (tests)
//!   record --shell-hidden <program> [args...]  through Explorer, hidden
//!
//! Ends after `secs` (default 15), or when a launched program has exited.

use std::time::{Duration, Instant};

use anabolic_core::activity::{ActivityKind, ActivityRecorder, LaunchHow, RecorderState};
use windows::Win32::Foundation::FILETIME;
use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};

fn cpu_time() -> Duration {
    let (mut c, mut e, mut k, mut u) = (
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
    );
    unsafe {
        let _ = GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u);
    }
    let t = |f: FILETIME| ((f.dwHighDateTime as u64) << 32 | f.dwLowDateTime as u64) * 100;
    Duration::from_nanos(t(k) + t(u))
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (mut recorder, launched, secs) = match args.first().map(String::as_str) {
        Some(flag @ ("--launch" | "--hidden" | "--shell-hidden")) => {
            let program = args.get(1).expect("a program to launch").clone();
            let rest = args[2..].join(" ");
            let how = match flag {
                "--hidden" => LaunchHow::Hidden,
                // Through Explorer like --launch, but SW_HIDE.
                "--shell-hidden" => LaunchHow::Normal { show: 0 },
                _ => LaunchHow::Normal { show: 1 },
            };
            (
                ActivityRecorder::launch_with(&program, &rest, None, how).expect("recording"),
                true,
                60,
            )
        }
        Some("--all") => {
            let secs = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(15);
            (
                ActivityRecorder::everything().expect("recording"),
                false,
                secs,
            )
        }
        Some(pid) => {
            let pid: u32 = pid.parse().expect("a pid");
            let secs = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(15);
            (
                ActivityRecorder::attach(pid).expect("recording"),
                false,
                secs,
            )
        }
        None => {
            eprintln!("record <pid> [secs] | record --launch <program> [args...]");
            return;
        }
    };
    let cpu_before = cpu_time();
    let started = Instant::now();
    let mut events = Vec::new();
    let mut alive_seen = false;
    // CPU spent getting ready (names, the start rundowns), apart from the
    // steady cost of recording.
    let mut ready_at: Option<(Duration, Instant)> = None;
    // RECORD_DISCARD: keep nothing, report memory now and then (does the
    // recorder itself grow?).
    let discard = std::env::var_os("RECORD_DISCARD").is_some();
    let mut taken = 0usize;
    let mut last_report = Instant::now();
    while started.elapsed() < Duration::from_secs(secs) {
        std::thread::sleep(Duration::from_millis(250));
        if discard {
            taken += recorder.take().len();
            if last_report.elapsed() >= Duration::from_secs(10) {
                last_report = Instant::now();
                println!(
                    "{:>4.0} s  {} rows taken  private {} MB",
                    started.elapsed().as_secs_f64(),
                    taken,
                    private_bytes() / 1_048_576
                );
            }
            continue;
        }
        events.extend(recorder.take());
        if ready_at.is_none() && recorder.state() == RecorderState::Recording {
            ready_at = Some((cpu_time().saturating_sub(cpu_before), Instant::now()));
        }
        match recorder.state() {
            RecorderState::Failed(why) => {
                eprintln!("failed: {why}");
                break;
            }
            RecorderState::Recording if launched => {
                // Done once every tracked process has exited.
                let starts = events
                    .iter()
                    .filter(|e| e.kind == ActivityKind::ProcessStart)
                    .count();
                let exits = events
                    .iter()
                    .filter(|e| e.kind == ActivityKind::ProcessExit)
                    .count();
                alive_seen |= starts > 0;
                if alive_seen && exits >= starts {
                    break;
                }
            }
            _ => {}
        }
    }
    let summary = recorder.summary();
    let tracked = recorder.tracked();
    events.extend(recorder.stop());
    let elapsed = started.elapsed();
    let cpu = cpu_time().saturating_sub(cpu_before);
    let all = args.first().is_some_and(|a| a == "--all");
    if all {
        whole_machine(&events, elapsed);
    }
    for e in events.iter().filter(|_| !all) {
        let at =
            e.at.duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default();
        let count = if e.count > 1 {
            format!(" ×{}", e.count)
        } else {
            String::new()
        };
        let bytes = if e.bytes > 0 {
            format!(" {} B", e.bytes)
        } else {
            String::new()
        };
        println!(
            "{:>6}.{:03} {:>6} {:<14} {:<18} {}{}{}  {}",
            at.as_secs() % 100_000,
            at.subsec_millis(),
            e.pid,
            e.kind.label(),
            e.result_text(),
            e.path,
            count,
            bytes,
            e.detail
        );
    }
    println!(
        "\n{} events kept, {} dropped, {} seen system-wide ({:.0}/s), {} lost by the kernel",
        summary.kept,
        summary.dropped,
        summary.seen,
        summary.seen as f64 / elapsed.as_secs_f64(),
        summary.lost
    );
    println!(
        "processes started {}, images {}, files created {} written {} deleted {}, values set {}, \
         keys created {} deleted {}, endpoints {} (sent {} B, received {} B); tracked {:?}",
        summary.processes_started,
        summary.images_loaded,
        summary.files_created,
        summary.files_written,
        summary.files_deleted,
        summary.values_set,
        summary.keys_created,
        summary.keys_deleted,
        summary.endpoints,
        summary.sent,
        summary.received,
        tracked
    );
    println!(
        "cost: {:.0} ms CPU in {:.1} s = {:.2} % of one core",
        cpu.as_secs_f64() * 1000.0,
        elapsed.as_secs_f64(),
        cpu.as_secs_f64() * 100.0 / elapsed.as_secs_f64()
    );
    if let Some((startup, at)) = ready_at {
        let steady = cpu.saturating_sub(startup);
        let span = (started + elapsed)
            .saturating_duration_since(at)
            .as_secs_f64()
            .max(0.001);
        println!(
            "  getting ready: {:.0} ms CPU; afterwards {:.2} % of one core",
            startup.as_secs_f64() * 1000.0,
            steady.as_secs_f64() * 100.0 / span
        );
    }
}

/// Private bytes of this process.
fn private_bytes() -> usize {
    use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX};
    let mut c = PROCESS_MEMORY_COUNTERS_EX::default();
    unsafe {
        let _ = GetProcessMemoryInfo(
            GetCurrentProcess(),
            (&mut c as *mut PROCESS_MEMORY_COUNTERS_EX).cast(),
            std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
        );
    }
    c.PrivateUsage
}

/// What a whole-machine recording amounts to: rates, kinds, the busiest
/// processes, memory.
fn whole_machine(events: &[anabolic_core::activity::ActivityEvent], elapsed: Duration) {
    use std::collections::HashMap;
    let secs = elapsed.as_secs_f64();
    println!(
        "{} events in {:.1} s = {:.0}/s; private bytes now {} MB; event struct {} B",
        events.len(),
        secs,
        events.len() as f64 / secs,
        private_bytes() / 1_048_576,
        std::mem::size_of::<anabolic_core::activity::ActivityEvent>()
    );
    let mut kinds: HashMap<&str, (u64, u64)> = HashMap::new();
    let mut pids: HashMap<u32, u64> = HashMap::new();
    let mut detail_bytes = 0usize;
    let mut path_bytes = 0usize;
    let mut unnamed = 0u64;
    for e in events {
        let k = kinds.entry(e.kind.label()).or_default();
        k.0 += 1;
        k.1 += e.count as u64;
        *pids.entry(e.pid).or_default() += 1;
        detail_bytes += e.detail.capacity();
        path_bytes += e.path.len();
        if e.path.starts_with('(') {
            unnamed += 1;
        }
    }
    println!(
        "detail heap {} MB, path text {} MB (shared or not), unnamed {}",
        detail_bytes / 1_048_576,
        path_bytes / 1_048_576,
        unnamed
    );
    // How much repeats: distinct paths, and rows that repeat the row
    // before them on the same thread (same operation, same path).
    let mut distinct: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut last: HashMap<u32, (&str, &str, &str)> = HashMap::new();
    let mut repeats = 0u64;
    let mut hot: HashMap<(u32, &str, &str), u64> = HashMap::new();
    for e in events {
        distinct.insert(&e.path);
        let now = (e.kind.label(), &*e.path, e.detail.as_str());
        if e.tid != 0 && last.insert(e.tid, now) == Some(now) {
            repeats += 1;
        }
        *hot.entry((e.pid, e.kind.label(), &*e.path)).or_default() += 1;
    }
    // Rows that repeat one the same process did within the last second
    // (same operation, path, detail and result), wherever they fall.
    let mut seen_at: HashMap<(u32, &str, &str, &str, Option<u32>), std::time::SystemTime> =
        HashMap::new();
    let mut foldable = [0u64; 3];
    for e in events {
        let k = (e.pid, e.kind.label(), &*e.path, e.detail.as_str(), e.status);
        if let Some(prev) = seen_at.get(&k) {
            let gap = e.at.duration_since(*prev).unwrap_or_default();
            if gap <= Duration::from_millis(100) {
                foldable[0] += 1;
            }
            if gap <= Duration::from_secs(1) {
                foldable[1] += 1;
            }
            if gap <= Duration::from_secs(5) {
                foldable[2] += 1;
            }
            if gap > Duration::from_secs(1) {
                seen_at.insert(k, e.at);
            }
        } else {
            seen_at.insert(k, e.at);
        }
    }
    println!(
        "foldable into an earlier row of the same process: within 0.1 s {}, 1 s {}, 5 s {}",
        foldable[0], foldable[1], foldable[2]
    );
    println!(
        "distinct paths {}, rows repeating their thread's previous row {}",
        distinct.len(),
        repeats
    );
    let mut hot: Vec<_> = hot.into_iter().collect();
    hot.sort_by_key(|h| std::cmp::Reverse(h.1));
    for ((pid, kind, path), n) in hot.into_iter().take(12) {
        println!("  {n:>7} {pid:>7} {kind:<12} {path}");
    }
    let mut starts: HashMap<(u32, &str), u64> = HashMap::new();
    for e in events
        .iter()
        .filter(|e| e.kind == anabolic_core::activity::ActivityKind::ProcessStart)
    {
        *starts.entry((e.pid, &*e.path)).or_default() += 1;
    }
    let mut starts: Vec<_> = starts.into_iter().collect();
    starts.sort_by_key(|s| std::cmp::Reverse(s.1));
    for ((parent, image), n) in starts.into_iter().take(8) {
        println!("  started {n:>4}x {image} by {parent}");
    }
    let mut kinds: Vec<_> = kinds.into_iter().collect();
    kinds.sort_by_key(|k| std::cmp::Reverse(k.1.0));
    for (k, (n, ops)) in kinds {
        println!("  {k:<16} {n:>9} rows  {ops:>10} operations");
    }
    let mut pids: Vec<_> = pids.into_iter().collect();
    pids.sort_by_key(|p| std::cmp::Reverse(p.1));
    let names = process_names();
    for (pid, n) in pids.into_iter().take(15) {
        println!(
            "  {:>7} {:<28} {:>9}",
            pid,
            names.get(&pid).map(String::as_str).unwrap_or("?"),
            n
        );
    }
}

fn process_names() -> std::collections::HashMap<u32, String> {
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };
    let mut out = std::collections::HashMap::new();
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
            return out;
        };
        let mut e = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut ok = Process32FirstW(snap, &mut e).is_ok();
        while ok {
            let len = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(0);
            out.insert(
                e.th32ProcessID,
                String::from_utf16_lossy(&e.szExeFile[..len]),
            );
            ok = Process32NextW(snap, &mut e).is_ok();
        }
        let _ = windows::Win32::Foundation::CloseHandle(snap);
    }
    out
}

//! Runs the frame and DPC/ISR traces for a while and prints what they saw,
//! with what tracing cost this process. Needs an elevated prompt.
//!
//! cargo run -p anabolic-core --example latency [seconds] [frames|latency]

use std::time::{Duration, Instant};

use anabolic_core::frames::FrameTracer;
use anabolic_core::latency::LatencyTracer;
use windows::Win32::Foundation::FILETIME;
use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};

fn cpu_time() -> Duration {
    let (mut a, mut b, mut kernel, mut user) = (
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
    );
    unsafe {
        let _ = GetProcessTimes(GetCurrentProcess(), &mut a, &mut b, &mut kernel, &mut user);
    }
    let t = |f: FILETIME| ((f.dwHighDateTime as u64) << 32) | f.dwLowDateTime as u64;
    Duration::from_nanos((t(kernel) + t(user)) * 100)
}

pub fn main() {
    let secs: u64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(15);
    let only = std::env::args().nth(2).unwrap_or_default();
    if !anabolic_core::hardening::is_elevated() {
        eprintln!("not elevated: both traces need administrator rights");
    }
    let _ =
        anabolic_core::hardening::enable_privilege(windows::core::w!("SeSystemProfilePrivilege"));
    let names = |pid: u32| {
        // Only for this printout: the image name from Toolhelp's process list.
        anabolic_core::winfile::nt_image_path(pid)
            .and_then(|p| p.rsplit('\\').next().map(std::sync::Arc::from))
    };
    let mut frames = (only != "latency").then(|| FrameTracer::start(true).expect("frames"));
    let mut latency = (only != "frames").then(|| LatencyTracer::start().expect("latency"));
    let cpu_before = cpu_time();
    let started = Instant::now();
    let mut frame_stats = None;
    let mut latency_stats = None;
    let mut take_time = Duration::ZERO;
    while started.elapsed() < Duration::from_secs(secs) {
        std::thread::sleep(Duration::from_secs(1));
        let t = Instant::now();
        if let Some(f) = &mut frames {
            frame_stats = Some(f.take(names));
        }
        if let Some(l) = &mut latency {
            latency_stats = Some(l.take());
        }
        take_time += t.elapsed();
    }
    let elapsed = started.elapsed();
    let cpu = cpu_time() - cpu_before;
    drop(frames);
    drop(latency);
    println!(
        "cost: {:.0} ms CPU in {:.1} s = {:.2} % of one core ({:.1} ms of it in take)",
        cpu.as_secs_f64() * 1000.0,
        elapsed.as_secs_f64(),
        cpu.as_secs_f64() * 100.0 / elapsed.as_secs_f64(),
        take_time.as_secs_f64() * 1000.0,
    );
    if let Some(f) = frame_stats {
        println!(
            "\n== frames (kernel presents: {}, lost {})",
            f.kernel_presents, f.lost_events
        );
        for p in &f.processes {
            println!(
                "{:>7} {:<28} {:<15} {:>6.1} fps  avg {:>6.2} ms  p99 {:>6.2} ms  1% low {:>6.1}  max {:>7.2} ms  stutters {:>3}  {}",
                p.pid,
                p.name,
                p.api.label(),
                p.fps,
                p.avg_ms,
                p.p99_ms,
                p.low_fps,
                p.max_ms,
                p.stutters,
                p.window
                    .as_ref()
                    .map_or("no window".to_string(), |w| format!(
                        "{} {}x{} @ {} Hz{}{}{}",
                        w.monitor,
                        w.width,
                        w.height,
                        w.refresh_hz,
                        if w.primary { " primary" } else { "" },
                        if w.fullscreen { " fullscreen" } else { "" },
                        if w.foreground { " foreground" } else { "" },
                    )),
            );
        }
        for s in f.stutters.iter().rev().take(10) {
            let ago = std::time::SystemTime::now()
                .duration_since(s.at)
                .unwrap_or_default();
            println!(
                "  stutter {:.1} s ago: {} ({}) {:.1} ms where {:.1} is typical",
                ago.as_secs_f64(),
                s.name,
                s.pid,
                s.frame_ms,
                s.typical_ms
            );
        }
    }
    if let Some(l) = latency_stats {
        println!(
            "\n== DPC/ISR over {:.1} s (unnamed: {}, lost {}, discarded {})",
            l.measured.as_secs_f64(),
            l.unnamed,
            l.lost_events,
            l.discarded
        );
        let total: f32 = l
            .drivers
            .iter()
            .map(|d| d.dpc.per_sec + d.isr.per_sec)
            .sum();
        println!("{total:.0} events/s in the last second");
        for d in l.drivers.iter().take(15) {
            println!(
                "{:<22} DPC {:>7}x {:>7.0}/s busy {:>5.2} %  max {:>7.1} µs {:?} | ISR {:>7}x {:>6.0}/s max {:>7.1} µs",
                d.driver,
                d.dpc.count,
                d.dpc.per_sec,
                d.dpc.busy * 100.0,
                d.dpc.highest_us,
                d.dpc.histogram,
                d.isr.count,
                d.isr.per_sec,
                d.isr.highest_us,
            );
        }
        let worst = l
            .per_cpu_highest_us
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1));
        if let Some((cpu, us)) = worst {
            println!("highest on any CPU: {us:.1} µs on CPU {cpu}");
        }
        for s in l.spikes.iter().rev().take(10) {
            println!(
                "  spike {:?} {} on CPU {}: {:.0} µs",
                s.kind, s.driver, s.cpu, s.us
            );
        }
    }
}

// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! The CPU view: total and per-core graphs, and one core's detail with who
//! ran on it.

use super::*;

/// Work interrupts leave for later: Windows' deferred procedure calls,
/// Linux's softirqs.
const DEFERRED: &str = if cfg!(windows) { "DPC" } else { "Softirq" };
const DEFERRED_PER_SEC: &str = if cfg!(windows) {
    "DPCs/s"
} else {
    "Softirqs/s"
};

pub(super) fn cpu(
    ui: &mut Ui,
    snap: &Snapshot,
    cpu_name: &str,
    topology: &Topology,
    selected: &mut Option<usize>,
    out: &mut Vec<UiCmd>,
) {
    let h = &snap.history;
    let s = &snap.system;
    title(ui, "CPU", cpu_name);
    big_graph(
        ui,
        Graph::new(Some(100.0))
            .series(&h.cpu, theme::CPU, true)
            .series(&h.cpu_kernel, theme::KERNEL, false),
        190.0,
        &h.cpu,
        snap,
        |m| format!("{m:.0} %"),
    );
    legend(ui, &[(theme::CPU, "Total"), (theme::KERNEL, "Kernel")]);
    ui.add_space(8.0);
    // The package's (Intel) or control temperature (AMD); Windows keeps it
    // for drivers.
    let temperature = anabolic_core::sensors::cpu_sensor(&s.sensors);
    band(
        ui,
        |ui| {
            let mut items = vec![
                ("Utilization", format!("{:.1} %", s.cpu.total)),
                ("Speed", ghz(s.cpu.mhz)),
                ("Kernel", format!("{:.1} %", s.cpu.kernel)),
                (DEFERRED, format!("{:.2} %", s.cpu.dpc)),
                ("Interrupts", format!("{:.2} %", s.cpu.interrupt)),
                ("Processes", fmt::count(s.processes as u64)),
                ("Threads", fmt::count(s.threads as u64)),
                (
                    if cfg!(windows) {
                        "Handles"
                    } else {
                        "Open files"
                    },
                    fmt::count(s.handles as u64),
                ),
                ("Up time", fmt::duration(s.uptime)),
                (
                    "Context switches/s",
                    fmt::count(s.cpu.context_switch_rate as u64),
                ),
                ("Interrupts/s", fmt::count(s.cpu.interrupt_rate as u64)),
            ]
            .into_iter()
            .map(|(caption, value)| (caption, RichText::new(value)))
            .collect::<Vec<_>>();
            if let Some(t) = temperature {
                items.insert(2, ("Temperature", reading_text(t)));
            }
            rich_stats(ui, &items)
        },
        |ui| {
            top(
                ui,
                snap,
                "Top CPU",
                |p| p.cpu as f64,
                |v| format!("{v:.2} %"),
                out,
            )
        },
    );
    ui.add_space(10.0);
    if let Some(t) = temperature
        && !h.cpu_temperature.is_empty()
    {
        ui.label(
            RichText::new(format!("Temperature ({})", t.label))
                .small()
                .color(theme::TEXT_WEAK),
        );
        temperature_graph(ui, snap, &h.cpu_temperature, t, theme::CPU, 80.0);
        ui.add_space(10.0);
    }

    // One small graph per logical processor.
    let activity = &s.core_activity;
    let items: Vec<Mini<'_>> = h
        .cores
        .iter()
        .enumerate()
        .map(|(i, ring)| Mini {
            label: match s.cores.get(i).map_or(0.0, |c| c.mhz) {
                mhz if mhz > 0.0 => format!("CPU {i}  ·  {:.2} GHz", mhz / 1000.0),
                _ => format!("CPU {i}"),
            },
            ring,
            value: s.cores.get(i).map_or(0.0, |c| c.total),
            // The busiest non-idle process on this core.
            caption: activity
                .get(i)
                .and_then(|a| a.processes.iter().find(|p| p.pid != 0 && p.share >= 1.0))
                .map(|p| p.name.to_string()),
        })
        .collect();
    if let Some(clicked) = mini_graphs(ui, &items, theme::CPU, *selected) {
        *selected = if *selected == Some(clicked) {
            None
        } else {
            Some(clicked)
        };
    }
    match &s.core_trace_error {
        Some(e) => {
            ui.label(
                RichText::new(format!("Per-core process view unavailable: {e}"))
                    .small()
                    .color(theme::ELEVATED_TEXT),
            );
        }
        None if selected.is_none() => {
            ui.label(
                RichText::new("Click a core to see what runs on it.")
                    .small()
                    .color(theme::TEXT_DIM),
            );
        }
        None => {}
    }
    if let Some(core) = *selected
        && core < h.cores.len()
    {
        core_detail(ui, snap, topology, core, out);
    }
}

/// "0-7, 16-23" for a set of processor indices.
pub(super) fn cpu_ranges(cpus: &[usize]) -> String {
    let mut sorted = cpus.to_vec();
    sorted.sort_unstable();
    let mut parts: Vec<String> = Vec::new();
    let mut i = 0;
    while i < sorted.len() {
        let start = sorted[i];
        let mut end = start;
        while i + 1 < sorted.len() && sorted[i + 1] == end + 1 {
            i += 1;
            end = sorted[i];
        }
        parts.push(if start == end {
            start.to_string()
        } else {
            format!("{start}–{end}")
        });
        i += 1;
    }
    parts.join(", ")
}

pub(super) fn cache_text(
    topology: &Topology,
    index: Option<usize>,
    name: &str,
    cpu: usize,
) -> Option<String> {
    let cache = topology.caches.get(index?)?;
    let shared: Vec<usize> = cache.cpus.iter().copied().filter(|&c| c != cpu).collect();
    let size = fmt::bytes(cache.size as u64);
    Some(if shared.is_empty() {
        format!("{name} {size}")
    } else {
        format!("{name} {size} shared with CPU {}", cpu_ranges(&shared))
    })
}

/// Everything about one logical processor: where it sits, how busy it is
/// and who has been running on it.
pub(super) fn core_detail(
    ui: &mut Ui,
    snap: &Snapshot,
    topology: &Topology,
    core: usize,
    out: &mut Vec<UiCmd>,
) {
    let h = &snap.history;
    let times = snap.system.cores.get(core).copied().unwrap_or_default();
    ui.add_space(12.0);
    let mut place = Vec::new();
    if let Some(c) = topology.cpus.get(core) {
        place.push(format!("physical core {}", c.core));
        if let Some(d) = topology.l3_domain(core)
            && topology.caches.iter().filter(|c| c.level == 3).count() > 1
        {
            place.push(format!("CCD {d}"));
        }
        if !c.siblings.is_empty() {
            place.push(format!("SMT sibling CPU {}", cpu_ranges(&c.siblings)));
        }
        place.push(format!("NUMA {}", c.numa));
        if topology
            .cpus
            .iter()
            .any(|o| o.efficiency_class != c.efficiency_class)
        {
            place.push(format!("efficiency class {}", c.efficiency_class));
        }
    }
    title(ui, &format!("CPU {core}"), &place.join("  ·  "));
    if let Some(c) = topology.cpus.get(core) {
        let caches: Vec<String> = [(c.l1d, "L1d"), (c.l2, "L2"), (c.l3, "L3")]
            .into_iter()
            .filter_map(|(i, n)| cache_text(topology, i, n, core))
            .collect();
        ui.label(
            RichText::new(caches.join("   ·   "))
                .small()
                .color(theme::TEXT_WEAK),
        );
    }
    if let (Some(total), Some(kernel)) = (h.cores.get(core), h.core_kernel.get(core)) {
        big_graph(
            ui,
            Graph::new(Some(100.0))
                .series(total, theme::CPU, true)
                .series(kernel, theme::KERNEL, false),
            130.0,
            total,
            snap,
            |m| format!("{m:.0} %"),
        );
        legend(ui, &[(theme::CPU, "Total"), (theme::KERNEL, "Kernel")]);
    }
    let activity = snap.system.core_activity.get(core);
    stats(
        ui,
        &[
            ("Utilization", format!("{:.1} %", times.total)),
            ("Clock", ghz(times.mhz)),
            ("Kernel", format!("{:.1} %", times.kernel)),
            (DEFERRED, format!("{:.2} %", times.dpc)),
            ("Interrupt time", format!("{:.2} %", times.interrupt)),
            ("Interrupts/s", fmt::count(times.interrupt_rate as u64)),
            (
                "Context switches/s",
                fmt::count(times.context_switch_rate as u64),
            ),
            (DEFERRED_PER_SEC, fmt::count(times.dpc_rate as u64)),
        ],
    );
    let empty = CoreActivity::default();
    let activity = activity.unwrap_or(&empty);
    ui.add_space(8.0);
    ui.label(
        RichText::new(format!("Who ran on CPU {core} during the last refresh"))
            .color(theme::TEXT_WEAK),
    );
    ui.label(
        RichText::new(
            "By process adds up all threads of a process. By thread lists the threads \
             themselves, so one busy thread (a game's main loop, a scanner's worker) stands \
             out. Click a thread to see where it started.",
        )
        .small()
        .color(theme::TEXT_DIM),
    );
    ui.add_space(4.0);
    let live = |pid: u32| {
        snap.processes
            .iter()
            .find(|q| q.pid == pid && q.state != ProcState::Exited && pid != 0)
    };
    let shown = |pid: u32, name: &str| {
        if pid == 0 {
            "Idle".to_string()
        } else {
            name.to_string()
        }
    };
    ui.horizontal_top(|ui| {
        ui.vertical(|ui| {
            widgets::section(ui, "By process");
            widgets::header(
                ui,
                &[
                    ("Process", 210.0, Align::Min),
                    ("PID", 64.0, Align::Max),
                    ("Share", 140.0, Align::Min),
                ],
            );
            let listed = activity.processes.len().min(CORE_ROWS);
            for (i, p) in activity.processes.iter().take(CORE_ROWS).enumerate() {
                widgets::row(ui, i, |ui| {
                    widgets::cell(ui, 210.0, Align::Min, |ui| {
                        let name = shown(p.pid, &p.name);
                        match live(p.pid) {
                            Some(q) => {
                                if widgets::link(ui, &name)
                                    .on_hover_text("Show in the process list")
                                    .clicked()
                                {
                                    out.push(UiCmd::GoTo(q.key));
                                }
                            }
                            None => {
                                ui.label(RichText::new(name).color(theme::TEXT_WEAK));
                            }
                        }
                    });
                    let pid = if p.pid == u32::MAX {
                        String::new()
                    } else {
                        p.pid.to_string()
                    };
                    widgets::text(
                        ui,
                        64.0,
                        Align::Max,
                        RichText::new(pid).color(theme::TEXT_DIM),
                    );
                    widgets::cell(ui, 140.0, Align::Min, |ui| {
                        share_bar(ui, p.share, p.pid == 0)
                    });
                });
            }
            widgets::blank_rows(ui, listed, CORE_ROWS, 414.0);
        });
        ui.add_space(36.0);
        ui.vertical(|ui| {
            widgets::section(ui, "By thread");
            widgets::header(
                ui,
                &[
                    ("Thread ID", 70.0, Align::Max),
                    ("Process", 200.0, Align::Min),
                    ("Share", 140.0, Align::Min),
                ],
            );
            let listed = activity.threads.len().min(CORE_ROWS);
            for (i, t) in activity.threads.iter().take(CORE_ROWS).enumerate() {
                widgets::row(ui, i, |ui| {
                    widgets::text(
                        ui,
                        70.0,
                        Align::Max,
                        RichText::new(t.tid.to_string())
                            .monospace()
                            .color(theme::TEXT_WEAK),
                    );
                    widgets::cell(ui, 200.0, Align::Min, |ui| {
                        let name = shown(t.pid, &t.name);
                        match live(t.pid) {
                            Some(q) => {
                                if widgets::link(ui, &name)
                                    .on_hover_text(format!(
                                        "Show thread {} of {} (start address, state)",
                                        t.tid, q.name
                                    ))
                                    .clicked()
                                {
                                    out.push(UiCmd::GoToThread {
                                        key: q.key,
                                        tid: t.tid,
                                    });
                                }
                            }
                            None => {
                                ui.label(RichText::new(name).color(theme::TEXT_WEAK));
                            }
                        }
                    });
                    widgets::cell(ui, 140.0, Align::Min, |ui| {
                        share_bar(ui, t.share, t.pid == 0)
                    });
                });
            }
            widgets::blank_rows(ui, listed, CORE_ROWS, 410.0);
        });
    });
}

pub(super) fn share_bar(ui: &mut Ui, share: f32, idle: bool) {
    let (rect, _) = ui.allocate_exact_size(vec2(80.0, 10.0), Sense::hover());
    ui.painter().rect_filled(rect, 2.0, theme::BG);
    let fill = Rect::from_min_size(
        rect.min,
        vec2(
            rect.width() * (share / 100.0).clamp(0.0, 1.0),
            rect.height(),
        ),
    );
    ui.painter()
        .rect_filled(fill, 2.0, if idle { theme::TEXT_DIM } else { theme::CPU });
    ui.label(RichText::new(format!("{share:.1}%")).small());
}

//! The disk views: every volume, and each physical disk with the processes
//! and files that use it.

use anabolic_core::disktrace::NAMES_FILES;

use super::*;

pub(super) fn disk(ui: &mut Ui, snap: &Snapshot, view: &mut PerfView, out: &mut Vec<UiCmd>) {
    let h = &snap.history;
    let s = &snap.system;
    title(ui, "Disk", "all physical disks");
    big_graph(
        ui,
        Graph::new(None)
            .series(&h.disk_read, theme::DISK_READ, true)
            .series(&h.disk_write, theme::DISK_WRITE, true),
        200.0,
        &h.disk_read,
        snap,
        |v| format!("{}/s", fmt::bytes(v as u64)),
    );
    legend(
        ui,
        &[(theme::DISK_READ, "Read"), (theme::DISK_WRITE, "Write")],
    );
    ui.add_space(10.0);
    stats(
        ui,
        &[
            ("Read", fmt::rate_always(s.disk_read_rate)),
            ("Write", fmt::rate_always(s.disk_write_rate)),
        ],
    );
    ui.add_space(10.0);
    let columns = [
        ("Disk", 280.0, Align::Min),
        ("Type", 110.0, Align::Min),
        ("Active", 70.0, Align::Max),
        ("Read", 100.0, Align::Max),
        ("Write", 100.0, Align::Max),
        ("Response", 80.0, Align::Max),
        ("Temp", 60.0, Align::Max),
        ("Busiest process", 220.0, Align::Min),
    ];
    widgets::header(ui, &columns);
    for (i, d) in s.disks.iter().enumerate() {
        widgets::row(ui, i, |ui| {
            widgets::cell(ui, columns[0].1, Align::Min, |ui| {
                let label = format!("Disk {}{}  ·  {}", d.number, volume_letters(d), d.model);
                if widgets::link(ui, &label).clicked() {
                    *view = PerfView::PhysicalDisk(d.number);
                }
            });
            widgets::text(
                ui,
                columns[1].1,
                Align::Min,
                RichText::new(disk_kind(d)).color(theme::TEXT_WEAK),
            );
            widgets::text(ui, columns[2].1, Align::Max, format!("{:.0} %", d.active));
            widgets::text(ui, columns[3].1, Align::Max, fmt::rate_always(d.read_rate));
            widgets::text(ui, columns[4].1, Align::Max, fmt::rate_always(d.write_rate));
            widgets::text(
                ui,
                columns[5].1,
                Align::Max,
                format!("{:.1} ms", d.response_ms),
            );
            widgets::text(
                ui,
                columns[6].1,
                Align::Max,
                d.temperature.map_or(String::new(), |t| format!("{t} °C")),
            );
            let busiest = s
                .disk_activity
                .iter()
                .find(|a| a.disk == d.number)
                .and_then(|a| a.processes.first())
                .filter(|p| p.io.rate() > 0.0);
            widgets::text(
                ui,
                columns[7].1,
                Align::Min,
                RichText::new(busiest.map_or(String::new(), |p| {
                    format!("{}  {}", p.name, fmt::rate_always(p.io.rate()))
                }))
                .color(theme::TEXT_WEAK),
            );
        });
    }
    if let [only] = s.disks.as_slice() {
        disk_users(ui, snap, only.number, out);
    } else if s.disk_tracing {
        ui.label(
            RichText::new(if NAMES_FILES {
                "Open a disk to see which processes and files use it."
            } else {
                "Open a disk to see which processes use it."
            })
            .small()
            .color(theme::TEXT_DIM),
        );
    }
    if snap.info_level != anabolic_core::InfoLevel::Full {
        ui.label(
            RichText::new(if cfg!(windows) {
                "Per-process disk counters need administrator rights."
            } else {
                "Other users' disk counters need cap_sys_ptrace: grant Anabolic its rights in \
                 Settings."
            })
            .color(theme::ELEVATED_TEXT),
        );
    }
    top(
        ui,
        snap,
        "Top disk (processes)",
        |p| p.disk_total_rate(),
        fmt::rate_always,
        out,
    );
}

pub(super) fn disk_kind(d: &anabolic_core::disk::DiskStats) -> String {
    let media = match d.ssd {
        Some(true) => "SSD",
        Some(false) => "HDD",
        None => "",
    };
    [d.bus, media]
        .iter()
        .filter(|s| !s.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join(" ")
}

pub(super) fn physical_disk(ui: &mut Ui, snap: &Snapshot, number: u32, out: &mut Vec<UiCmd>) {
    let Some(d) = snap.system.disks.iter().find(|d| d.number == number) else {
        ui.label(RichText::new("This disk is gone.").color(theme::TEXT_WEAK));
        return;
    };
    title(
        ui,
        &format!("Disk {}{}", d.number, volume_letters(d)),
        &d.model,
    );
    ui.label(
        RichText::new(format!(
            "{}  ·  {}{}",
            disk_kind(d),
            fmt::bytes(d.size),
            d.temperature
                .map_or(String::new(), |t| format!("  ·  {t} °C"))
        ))
        .color(theme::TEXT_WEAK),
    );
    if let Some(dh) = snap.history.disk(number) {
        ui.label(RichText::new("Active time").small().color(theme::TEXT_WEAK));
        big_graph(
            ui,
            Graph::new(Some(100.0)).series(&dh.active, theme::DISK_WRITE, true),
            110.0,
            &dh.active,
            snap,
            |m| format!("{m:.0} %"),
        );
        ui.label(
            RichText::new("Transfer rate")
                .small()
                .color(theme::TEXT_WEAK),
        );
        big_graph(
            ui,
            Graph::new(None)
                .series(&dh.read, theme::DISK_READ, true)
                .series(&dh.write, theme::DISK_WRITE, true),
            150.0,
            &dh.read,
            snap,
            |v| format!("{}/s", fmt::bytes(v as u64)),
        );
        legend(
            ui,
            &[(theme::DISK_READ, "Read"), (theme::DISK_WRITE, "Write")],
        );
    }
    ui.add_space(8.0);
    stats(
        ui,
        &[
            ("Active time", format!("{:.1} %", d.active)),
            ("Average response", format!("{:.2} ms", d.response_ms)),
            ("Read", fmt::rate_always(d.read_rate)),
            ("Write", fmt::rate_always(d.write_rate)),
            ("Read IOPS", fmt::count(d.read_iops as u64)),
            ("Write IOPS", fmt::count(d.write_iops as u64)),
            ("Queue depth", d.queue.to_string()),
            (
                "Temperature",
                d.temperature.map_or("—".into(), |t| format!("{t} °C")),
            ),
        ],
    );
    disk_users(ui, snap, number, out);
    if d.volumes.is_empty() {
        return;
    }
    ui.add_space(14.0);
    ui.label(RichText::new("Volumes").color(theme::TEXT_WEAK));
    let columns = [
        ("Volume", 70.0, Align::Min),
        ("Label", 180.0, Align::Min),
        ("Used", 200.0, Align::Min),
        ("Free", 100.0, Align::Max),
        ("Total", 100.0, Align::Max),
    ];
    widgets::header(ui, &columns);
    for (i, v) in d.volumes.iter().enumerate() {
        widgets::row(ui, i, |ui| {
            widgets::text(ui, columns[0].1, Align::Min, v.letter.clone());
            widgets::text(
                ui,
                columns[1].1,
                Align::Min,
                RichText::new(v.label.clone()).color(theme::TEXT_WEAK),
            );
            widgets::cell(ui, columns[2].1, Align::Min, |ui| {
                let used = v.total.saturating_sub(v.free);
                let fraction = if v.total > 0 {
                    used as f32 / v.total as f32
                } else {
                    0.0
                };
                let (rect, _) = ui.allocate_exact_size(vec2(140.0, 10.0), Sense::hover());
                ui.painter().rect_filled(rect, 2.0, theme::BG);
                let color = if fraction > 0.9 {
                    theme::BAD
                } else {
                    theme::CPU
                };
                ui.painter().rect_filled(
                    Rect::from_min_size(rect.min, vec2(rect.width() * fraction, rect.height())),
                    2.0,
                    color,
                );
                ui.label(RichText::new(format!("{:.0} %", fraction * 100.0)).small());
            });
            widgets::text(ui, columns[3].1, Align::Max, fmt::bytes(v.free));
            widgets::text(
                ui,
                columns[4].1,
                Align::Max,
                RichText::new(fmt::bytes(v.total)).color(theme::TEXT_WEAK),
            );
        });
    }
}

/// A process name that opens it in the process list while it still runs.
pub(super) fn process_link(
    ui: &mut Ui,
    snap: &Snapshot,
    pid: u32,
    name: &str,
    out: &mut Vec<UiCmd>,
) {
    let live = snap
        .processes
        .iter()
        .find(|p| p.pid == pid && p.state != ProcState::Exited && !p.is_pseudo());
    match live {
        Some(p) => {
            if widgets::link(ui, name)
                .on_hover_text("Show in the process list")
                .clicked()
            {
                out.push(UiCmd::GoTo(p.key));
            }
        }
        None => {
            ui.label(RichText::new(name).color(theme::TEXT_WEAK));
        }
    }
}

pub(super) fn disk_use_cells(ui: &mut Ui, io: &DiskUse, widths: [f32; 3]) {
    // Blank means zero, as in the process list.
    let rate = |v: f64| {
        if v > 0.0 {
            RichText::new(fmt::rate_always(v))
        } else {
            RichText::new("")
        }
    };
    widgets::text(ui, widths[0], Align::Max, rate(io.read_rate));
    widgets::text(ui, widths[1], Align::Max, rate(io.write_rate));
    widgets::text(
        ui,
        widths[2],
        Align::Max,
        RichText::new(format!("{:.1} ms", io.response_ms)).color(theme::TEXT_WEAK),
    );
}

/// Which processes and files read and write one disk, from the disk trace.
pub(super) fn disk_users(ui: &mut Ui, snap: &Snapshot, number: u32, out: &mut Vec<UiCmd>) {
    let s = &snap.system;
    ui.add_space(14.0);
    ui.label(RichText::new("What is using this disk").color(theme::TEXT_WEAK));
    let empty = DiskActivity::default();
    let activity = s
        .disk_activity
        .iter()
        .find(|a| a.disk == number)
        .unwrap_or(&empty);
    // Always one status line and full-height tables, filled or not: the
    // page must not change height as activity comes and goes.
    let status = match &s.disk_trace_error {
        Some(e) => RichText::new(format!("Unavailable: {e}")).color(theme::ELEVATED_TEXT),
        None if !s.disk_tracing => RichText::new("Starting the disk trace…").color(theme::TEXT_DIM),
        None if activity.processes.is_empty() => {
            RichText::new("No reads or writes on this disk since this view opened.")
                .color(theme::TEXT_DIM)
        }
        None => RichText::new("Busiest first.").color(theme::TEXT_DIM),
    };
    ui.label(status.small());

    let columns = [
        ("Process", 220.0, Align::Min),
        ("PID", 64.0, Align::Max),
        ("Read", 100.0, Align::Max),
        ("Write", 100.0, Align::Max),
        ("Response", 80.0, Align::Max),
        ("Total", 100.0, Align::Max),
    ];
    widgets::header(ui, &columns);
    let width: f32 = columns.iter().map(|c| c.1).sum();
    let shown = activity.processes.len().min(DISK_PROCESS_ROWS);
    for (i, p) in activity
        .processes
        .iter()
        .take(DISK_PROCESS_ROWS)
        .enumerate()
    {
        widgets::row(ui, i, |ui| {
            widgets::cell(ui, columns[0].1, Align::Min, |ui| {
                process_link(ui, snap, p.pid, &p.name, out)
            });
            let pid = if p.pid == u32::MAX {
                String::new()
            } else {
                p.pid.to_string()
            };
            widgets::text(
                ui,
                columns[1].1,
                Align::Max,
                RichText::new(pid).color(theme::TEXT_DIM),
            );
            disk_use_cells(ui, &p.io, [columns[2].1, columns[3].1, columns[4].1]);
            widgets::text(
                ui,
                columns[5].1,
                Align::Max,
                RichText::new(fmt::bytes(p.io.read_total + p.io.write_total))
                    .color(theme::TEXT_WEAK),
            );
        });
    }
    widgets::blank_rows(ui, shown, DISK_PROCESS_ROWS, width);

    ui.add_space(10.0);
    if !NAMES_FILES {
        ui.label(
            RichText::new(
                "Which files: the kernel's block layer, where disks are traced, knows sectors, \
                 not files. Averaged over a few seconds. Cached writes reach the disk later \
                 through the kernel's flusher threads, counted under Kernel.",
            )
            .small()
            .color(theme::TEXT_DIM),
        );
        return;
    }
    ui.label(RichText::new("Files").color(theme::TEXT_WEAK));
    let columns = [
        ("File", 440.0, Align::Min),
        ("Process", 170.0, Align::Min),
        ("Read", 90.0, Align::Max),
        ("Write", 90.0, Align::Max),
        ("Response", 80.0, Align::Max),
    ];
    widgets::header(ui, &columns);
    let width: f32 = columns.iter().map(|c| c.1).sum();
    let shown = activity.files.len().min(DISK_FILE_ROWS);
    for (i, f) in activity.files.iter().take(DISK_FILE_ROWS).enumerate() {
        widgets::row(ui, i, |ui| {
            widgets::cell(ui, columns[0].1, Align::Min, |ui| {
                let hover = if &*f.path == anabolic_core::diskusage::UNNAMED {
                    "Opened before this view was, and closed again (or file system \
                     metadata) before the kernel could be asked for its name."
                } else {
                    &f.path
                };
                ui.add(egui::Label::new(&*f.path).truncate())
                    .on_hover_text(hover);
            });
            widgets::cell(ui, columns[1].1, Align::Min, |ui| {
                process_link(ui, snap, f.pid, &f.process, out)
            });
            disk_use_cells(ui, &f.io, [columns[2].1, columns[3].1, columns[4].1]);
        });
    }
    widgets::blank_rows(ui, shown, DISK_FILE_ROWS, width);
    ui.label(
        RichText::new(
            "Averaged over a few seconds. Cached writes reach the disk later through System; \
             the file column says whose data it is.",
        )
        .small()
        .color(theme::TEXT_DIM),
    );
}

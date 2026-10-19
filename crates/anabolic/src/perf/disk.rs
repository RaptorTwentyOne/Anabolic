//! The disk views: every volume, and each physical disk with its
//! partitions and the processes and files that use it.

use anabolic_core::disk::{DiskStats, Partition, PartitionRole};
use anabolic_core::disktrace::NAMES_FILES;
use eframe::egui::Color32;

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
    // A single disk has no card of its own: what its view shows comes here.
    if let [only] = s.disks.as_slice() {
        if !only.partitions.is_empty() {
            partitions(ui, only);
        }
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
    if d.partitions.is_empty() {
        volumes(ui, d);
    } else {
        partitions(ui, d);
    }
    disk_users(ui, snap, number, out);
}

/// How full a file system is: a bar and the share.
fn used_bar(ui: &mut Ui, total: u64, free: u64, width: f32) {
    let used = total.saturating_sub(free);
    let fraction = if total > 0 {
        used as f32 / total as f32
    } else {
        0.0
    };
    let (rect, _) = ui.allocate_exact_size(vec2(width, 10.0), Sense::hover());
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
}

/// The file systems on a disk without a partition table.
fn volumes(ui: &mut Ui, d: &DiskStats) {
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
                used_bar(ui, v.total, v.free, 140.0)
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

fn role_color(role: PartitionRole) -> Color32 {
    match role {
        PartitionRole::Data => theme::CPU,
        PartitionRole::Boot => theme::DISK_WRITE,
        PartitionRole::Recovery => theme::TEXT_DIM,
        PartitionRole::Reserved | PartitionRole::Extended => theme::BORDER_STRONG,
        PartitionRole::Swap => theme::MEMORY,
        PartitionRole::Encrypted => theme::NET_TX,
        PartitionRole::Pool => theme::NET_RX,
    }
}

/// What a partition goes by: where it is mounted, else its label, its
/// name or its type.
fn partition_title(p: &Partition) -> &str {
    [
        p.mounts.first().map_or("", String::as_str),
        &p.label,
        &p.name,
        &p.kind,
    ]
    .into_iter()
    .find(|s| !s.is_empty())
    .unwrap_or("")
}

fn partition_details(p: &Partition) -> String {
    let mut lines = vec![if p.device.is_empty() {
        format!("Partition {}", p.number)
    } else {
        format!("Partition {}  ·  {}", p.number, p.device)
    }];
    lines.push(format!("{}  ·  {}", p.kind, p.role.name()));
    for (caption, value) in [
        ("Name", &p.name),
        ("Label", &p.label),
        ("File system", &p.file_system),
    ] {
        if !value.is_empty() {
            lines.push(format!("{caption}: {value}"));
        }
    }
    if !p.mounts.is_empty() {
        lines.push(format!("Mounted at {}", p.mounts.join(", ")));
    }
    lines.push(if p.total > 0 {
        format!(
            "{}, {} free",
            fmt::bytes(p.size),
            fmt::bytes(p.free.min(p.total))
        )
    } else {
        fmt::bytes(p.size)
    });
    if p.system {
        lines.push("Holds the running system".into());
    }
    lines.join("\n")
}

/// The partitions in the order they sit on the disk, each as wide as its
/// share (but never too thin to see), the part in use brighter.
fn partition_map(ui: &mut Ui, d: &DiskStats) {
    enum Piece<'a> {
        Partition(&'a Partition),
        Unallocated(u64),
    }
    let size = |piece: &Piece| match piece {
        Piece::Partition(p) => p.size,
        Piece::Unallocated(n) => *n,
    };
    // Alignment leaves a little room between partitions everywhere.
    let floor = (d.size / 1000).max(16 << 20);
    let mut pieces = Vec::new();
    let mut end = 0;
    for p in &d.partitions {
        if p.offset.saturating_sub(end) >= floor {
            pieces.push(Piece::Unallocated(p.offset - end));
        }
        pieces.push(Piece::Partition(p));
        end = end.max(p.offset + p.size);
    }
    if d.size.saturating_sub(end) >= floor {
        pieces.push(Piece::Unallocated(d.size - end));
    }

    const GAP: f32 = 2.0;
    const THINNEST: f32 = 6.0;
    const STRIP: f32 = 4.0;
    let height = 40.0;
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
    let n = pieces.len() as f32;
    let room = (rect.width() - GAP * (n - 1.0) - THINNEST * n).max(0.0);
    let whole = pieces.iter().map(size).sum::<u64>().max(1) as f32;
    let mut x = rect.left();
    for (i, piece) in pieces.iter().enumerate() {
        let w = THINNEST + room * size(piece) as f32 / whole;
        let r = Rect::from_min_size(egui::pos2(x, rect.top()), vec2(w, height));
        x += w + GAP;
        let painter = ui.painter_at(r);
        let (lines, hover) = match piece {
            Piece::Unallocated(n) => {
                painter.rect_stroke(
                    r,
                    3.0,
                    egui::Stroke::new(1.0, theme::BORDER_STRONG),
                    egui::StrokeKind::Inside,
                );
                let text = format!("Unallocated  ·  {}", fmt::bytes(*n));
                (
                    [
                        ("Unallocated".to_string(), theme::TEXT_DIM),
                        (fmt::bytes(*n), theme::TEXT_DIM),
                    ],
                    text,
                )
            }
            Piece::Partition(p) => {
                let color = role_color(p.role);
                painter.rect_filled(r, 3.0, color.gamma_multiply(0.18));
                if p.total > 0 {
                    let used = p.total.saturating_sub(p.free) as f32 / p.total as f32;
                    let fill = Rect::from_min_max(
                        egui::pos2(r.left(), r.top() + STRIP),
                        egui::pos2(r.left() + r.width() * used.clamp(0.0, 1.0), r.bottom()),
                    );
                    painter.rect_filled(
                        fill,
                        egui::CornerRadius {
                            sw: 3,
                            se: if used > 0.99 { 3 } else { 0 },
                            ..Default::default()
                        },
                        color.gamma_multiply(0.45),
                    );
                }
                painter.rect_filled(
                    Rect::from_min_size(r.min, vec2(r.width(), STRIP)),
                    egui::CornerRadius {
                        nw: 3,
                        ne: 3,
                        ..Default::default()
                    },
                    color,
                );
                if p.system {
                    painter.rect_stroke(
                        r,
                        3.0,
                        egui::Stroke::new(1.0, theme::ACCENT),
                        egui::StrokeKind::Inside,
                    );
                }
                (
                    [
                        (partition_title(p).to_string(), theme::TEXT),
                        (fmt::bytes(p.size), theme::TEXT_WEAK),
                    ],
                    partition_details(p),
                )
            }
        };
        // Written where it fits; the rest is in the hover and the table.
        let mut y = r.top() + STRIP + 3.0;
        for (j, (text, color)) in lines.into_iter().enumerate() {
            let size = if j == 0 { theme::META } else { theme::CAPTION };
            let galley = painter.layout_no_wrap(text, FontId::proportional(size), color);
            let line = galley.size().y;
            if galley.size().x <= r.width() - 10.0 {
                painter.galley(egui::pos2(r.left() + 5.0, y), galley, color);
            }
            y += line;
        }
        ui.interact(r, ui.id().with(("partition", i)), Sense::hover())
            .on_hover_text(hover);
    }

    // What the colours mean, for the kinds on this disk.
    let mut roles: Vec<PartitionRole> = Vec::new();
    for p in &d.partitions {
        if !roles.contains(&p.role) {
            roles.push(p.role);
        }
    }
    ui.horizontal(|ui| {
        for role in roles {
            let (r, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
            ui.painter().rect_filled(r, 2.0, role_color(role));
            ui.label(RichText::new(role.name()).small().color(theme::TEXT_WEAK));
            ui.add_space(8.0);
        }
        if pieces.iter().any(|p| matches!(p, Piece::Unallocated(_))) {
            let (r, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
            ui.painter().rect_stroke(
                r,
                2.0,
                egui::Stroke::new(1.0, theme::BORDER_STRONG),
                egui::StrokeKind::Inside,
            );
            ui.label(RichText::new("Unallocated").small().color(theme::TEXT_WEAK));
            ui.add_space(8.0);
        }
        ui.label(
            RichText::new("Brighter: in use")
                .small()
                .color(theme::TEXT_DIM),
        );
    });
}

fn partitions(ui: &mut Ui, d: &DiskStats) {
    ui.add_space(14.0);
    let style = d.style.name();
    ui.label(
        RichText::new(if style.is_empty() {
            "Partitions".to_string()
        } else {
            format!("Partitions  ·  {style}")
        })
        .color(theme::TEXT_WEAK),
    );
    partition_map(ui, d);
    ui.add_space(6.0);
    let columns = [
        ("#", 44.0, Align::Min),
        ("Name", 170.0, Align::Min),
        ("Type", 150.0, Align::Min),
        ("File system", 100.0, Align::Min),
        ("Mounted at", 130.0, Align::Min),
        ("Used", 140.0, Align::Min),
        ("Free", 76.0, Align::Max),
        ("Size", 80.0, Align::Max),
    ];
    widgets::header(ui, &columns);
    for (i, p) in d.partitions.iter().enumerate() {
        widgets::row(ui, i, |ui| {
            widgets::cell(ui, columns[0].1, Align::Min, |ui| {
                let (r, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
                ui.painter().rect_filled(r, 2.0, role_color(p.role));
                ui.label(p.number.to_string());
            });
            // The file system's label, else the name in the partition table.
            let name = if p.label.is_empty() {
                RichText::new(&p.name).color(theme::TEXT_WEAK)
            } else {
                RichText::new(&p.label)
            };
            widgets::cell(ui, columns[1].1, Align::Min, |ui| {
                ui.add(egui::Label::new(name).truncate())
                    .on_hover_text(partition_details(p));
            });
            widgets::text(
                ui,
                columns[2].1,
                Align::Min,
                RichText::new(&p.kind).color(theme::TEXT_WEAK),
            );
            widgets::text(ui, columns[3].1, Align::Min, p.file_system.clone());
            widgets::text(
                ui,
                columns[4].1,
                Align::Min,
                RichText::new(p.mounts.join(", ")).color(if p.system {
                    theme::ACCENT
                } else {
                    theme::TEXT
                }),
            );
            widgets::cell(ui, columns[5].1, Align::Min, |ui| {
                if p.total > 0 {
                    used_bar(ui, p.total, p.free, 80.0);
                }
            });
            let free = if p.total > 0 {
                fmt::bytes(p.free.min(p.total))
            } else {
                String::new()
            };
            widgets::text(ui, columns[6].1, Align::Max, free);
            widgets::text(
                ui,
                columns[7].1,
                Align::Max,
                RichText::new(fmt::bytes(p.size)).color(theme::TEXT_WEAK),
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

//! Performance tab: system-wide graphs plus the processes behind them.

use std::sync::Arc;

use anabolic_core::handles::ZombieReport;
use anabolic_core::history::Ring;
use anabolic_core::hwinfo::HardwareInfo;
use anabolic_core::topology::Topology;
use anabolic_core::{CoreActivity, DiskActivity, DiskUse, ProcState, ProcessInfo, Snapshot};
use eframe::egui::{self, Align, Align2, FontId, Layout, Rect, RichText, Sense, Ui, vec2};

use crate::fmt;
use crate::graph::Graph;
use crate::proc_table::UiCmd;
use crate::theme;
use crate::widgets;

mod cpu;
mod disk;
mod info;
use cpu::*;
use disk::*;
use info::*;

/// Result of the on-demand "who keeps exited processes alive" scan.
#[derive(Default)]
pub struct ZombieView {
    pub report: Option<Arc<ZombieReport>>,
    pub pending: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum PerfView {
    #[default]
    Cpu,
    CpuInfo,
    Memory,
    MemoryInfo,
    Disk,
    PhysicalDisk(u32),
    Network,
    Adapter(u64),
    Io,
    Gpu(usize),
    Displays,
}

#[allow(clippy::too_many_arguments)]
pub fn show(
    ui: &mut Ui,
    view: &mut PerfView,
    core: &mut Option<usize>,
    snap: &Snapshot,
    cpu_name: &str,
    hw: &HardwareInfo,
    zombies: &ZombieView,
    displays: Option<&[anabolic_core::displays::Display]>,
    out: &mut Vec<UiCmd>,
) {
    egui::Panel::left("perf-nav")
        .exact_size(240.0)
        .resizable(false)
        .show(ui, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.add_space(6.0);
                    nav(ui, view, snap, hw, displays);
                });
        });
    egui::CentralPanel::default_margins().show(ui, |ui| {
        let mut area = egui::ScrollArea::vertical().auto_shrink([false, false]);
        if let Some(offset) = crate::bench::scroll_offset(ui.ctx()) {
            area = area.vertical_scroll_offset(offset);
        }
        let output = area.show(ui, |ui| match *view {
            PerfView::Cpu => cpu(ui, snap, cpu_name, &hw.topology, core, out),
            PerfView::CpuInfo => cpu_info(ui, snap, hw),
            PerfView::Memory => memory(ui, snap, zombies, out),
            PerfView::MemoryInfo => memory_info(ui, snap, hw),
            PerfView::Disk => disk(ui, snap, view, out),
            PerfView::PhysicalDisk(n) => physical_disk(ui, snap, n, out),
            PerfView::Network => network(ui, snap, view, out),
            PerfView::Adapter(luid) => adapter(ui, snap, luid),
            PerfView::Io => io(ui, snap, out),
            PerfView::Gpu(i) => gpu(ui, snap, i, out),
            PerfView::Displays => crate::displays_view::show(ui, displays),
        });
        crate::bench::scrolled_to(output.state.offset.y, output.content_size.y);
    });
}

fn ghz(mhz: f32) -> String {
    if mhz > 0.0 {
        format!("{:.2} GHz", mhz / 1000.0)
    } else {
        "—".into()
    }
}

/// One entry of the navigation tree.
struct Card<'a> {
    view: PerfView,
    title: String,
    value: String,
    graph: Option<Graph<'a>>,
    child: bool,
}

fn volume_letters(d: &anabolic_core::disk::DiskStats) -> String {
    let letters: Vec<&str> = d.volumes.iter().map(|v| v.letter.as_str()).collect();
    if letters.is_empty() {
        String::new()
    } else {
        format!(" ({})", letters.join(" "))
    }
}

fn nav(
    ui: &mut Ui,
    view: &mut PerfView,
    snap: &Snapshot,
    hw: &HardwareInfo,
    displays: Option<&[anabolic_core::displays::Display]>,
) {
    let h = &snap.history;
    let s = &snap.system;
    let mem = &s.memory;
    let mut cards: Vec<Card<'_>> = Vec::new();
    let top = |view, title: &str, value: String, graph| Card {
        view,
        title: title.into(),
        value,
        graph: Some(graph),
        child: false,
    };

    cards.push(top(
        PerfView::Cpu,
        "CPU",
        format!("{:.0} %  ·  {}", s.cpu.total, ghz(s.cpu.mhz)),
        Graph::new(Some(100.0)).series(&h.cpu, theme::CPU, true),
    ));
    cards.push(Card {
        view: PerfView::CpuInfo,
        title: "Processor info".into(),
        value: format!("{} cores  ·  {} threads", hw.cpu.cores, hw.cpu.threads),
        graph: None,
        child: true,
    });
    cards.push(top(
        PerfView::Memory,
        "Memory",
        format!(
            "{} / {} ({:.0} %)",
            fmt::bytes(mem.used()),
            fmt::bytes(mem.total),
            pct(mem.used(), mem.total)
        ),
        Graph::new(Some(mem.total.max(1) as f32)).series(&h.memory_used, theme::MEMORY, true),
    ));
    let modules = &hw.memory.modules;
    cards.push(Card {
        view: PerfView::MemoryInfo,
        title: "Modules".into(),
        value: match modules.first() {
            Some(m) => format!(
                "{} × {}  {}  ·  {} MT/s",
                modules.len(),
                fmt::bytes(m.size),
                m.kind,
                if m.configured_speed > 0 {
                    m.configured_speed
                } else {
                    m.speed
                }
            ),
            None => "—".into(),
        },
        graph: None,
        child: true,
    });
    cards.push(top(
        PerfView::Disk,
        "Disk",
        format!(
            "R {}  W {}",
            fmt::rate_always(s.disk_read_rate),
            fmt::rate_always(s.disk_write_rate)
        ),
        Graph::new(None)
            .series(&h.disk_read, theme::DISK_READ, true)
            .series(&h.disk_write, theme::DISK_WRITE, false),
    ));
    // A single device adds nothing over its parent; only list several.
    for d in s.disks.iter().filter(|_| s.disks.len() > 1) {
        if let Some(dh) = h.disk(d.number) {
            cards.push(Card {
                view: PerfView::PhysicalDisk(d.number),
                title: format!("Disk {}{}", d.number, volume_letters(d)),
                value: format!(
                    "{:.0} %  ·  {}",
                    d.active,
                    fmt::rate_always(d.read_rate + d.write_rate)
                ),
                graph: Some(Graph::new(Some(100.0)).series(&dh.active, theme::DISK_WRITE, true)),
                child: true,
            });
        }
    }
    cards.push(top(
        PerfView::Network,
        "Network",
        format!(
            "R {}  S {}",
            fmt::rate_always(s.net.rx_rate),
            fmt::rate_always(s.net.tx_rate)
        ),
        Graph::new(None)
            .series(&h.net_rx, theme::NET_RX, true)
            .series(&h.net_tx, theme::NET_TX, false),
    ));
    for a in s.net.adapters.iter().filter(|_| s.net.adapters.len() > 1) {
        if let Some(ah) = h.adapter(a.luid) {
            cards.push(Card {
                view: PerfView::Adapter(a.luid),
                title: a.name.clone(),
                value: format!(
                    "R {}  S {}",
                    fmt::rate_always(a.rx_rate),
                    fmt::rate_always(a.tx_rate)
                ),
                graph: Some(Graph::new(None).series(&ah.rx, theme::NET_RX, true).series(
                    &ah.tx,
                    theme::NET_TX,
                    false,
                )),
                child: true,
            });
        }
    }
    cards.push(top(
        PerfView::Io,
        "Process I/O",
        fmt::rate_always(s.io_read_rate + s.io_write_rate + s.io_other_rate),
        Graph::new(None)
            .series(&h.io_read, theme::DISK_READ, true)
            .series(&h.io_write, theme::DISK_WRITE, false),
    ));
    for (i, (gpu, gh)) in s.gpus.iter().zip(&h.gpus).enumerate() {
        cards.push(top(
            PerfView::Gpu(i),
            &format!("GPU {i}"),
            format!("{:.0} %  ·  {}", gpu.usage, short_gpu_name(&gpu.name)),
            Graph::new(Some(100.0)).series(&gh.usage, theme::GPU, true),
        ));
    }
    cards.push(Card {
        view: PerfView::Displays,
        title: "Displays".into(),
        value: match displays {
            Some(d) => {
                let flagged = d
                    .iter()
                    .filter(|d| d.below_max() || d.below_native())
                    .count();
                let main = d.iter().find(|d| d.primary).map_or(String::new(), |m| {
                    format!("  ·  main {:.0} Hz", m.refresh_hz)
                });
                if flagged > 0 {
                    format!("{} monitors  ·  {flagged} to look at", d.len())
                } else {
                    format!(
                        "{} monitor{}{main}",
                        d.len(),
                        if d.len() == 1 { "" } else { "s" }
                    )
                }
            }
            None => "Monitors and GPUs".into(),
        },
        graph: None,
        child: false,
    });

    for card in cards {
        let (height, indent) = if card.child {
            (44.0, 16.0)
        } else {
            (62.0, 0.0)
        };
        let (outer, response) =
            ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::click());
        let rect = Rect::from_min_max(outer.min + vec2(indent, 0.0), outer.max);
        let bg = if *view == card.view {
            theme::SELECTION
        } else if response.hovered() {
            theme::RAISED
        } else {
            theme::PANEL
        };
        ui.painter().rect_filled(rect, 4.0, bg);
        if card.child {
            // Tree guide.
            let x = outer.left() + 7.0;
            ui.painter()
                .vline(x, outer.y_range(), egui::Stroke::new(1.0, theme::BORDER));
            ui.painter().hline(
                x..=rect.left(),
                rect.center().y,
                egui::Stroke::new(1.0, theme::BORDER),
            );
        }
        let text_x = match card.graph {
            Some(graph) => {
                let size = if card.child {
                    vec2(52.0, 30.0)
                } else {
                    vec2(72.0, 44.0)
                };
                let g = Rect::from_min_size(rect.min + vec2(8.0, (height - size.y) / 2.0), size);
                graph.no_grid().paint(ui, g);
                g.right() + 10.0
            }
            None => rect.left() + 10.0,
        };
        // Clip the text to the card.
        let painter = ui.painter_at(rect.shrink(2.0));
        let (title_font, value_size, title_y, value_y) = if card.child {
            (FontId::proportional(theme::BODY), theme::CAPTION, 6.0, 25.0)
        } else {
            (
                theme::semibold(theme::BODY + 1.0),
                theme::CAPTION,
                12.0,
                33.0,
            )
        };
        painter.text(
            egui::pos2(text_x, rect.top() + title_y),
            Align2::LEFT_TOP,
            &card.title,
            title_font,
            theme::TEXT,
        );
        painter.text(
            egui::pos2(text_x, rect.top() + value_y),
            Align2::LEFT_TOP,
            &card.value,
            FontId::proportional(value_size),
            theme::TEXT_WEAK,
        );
        if response.clicked() {
            *view = card.view;
        }
        ui.add_space(if card.child { 2.0 } else { 4.0 });
    }
}

/// "NVIDIA GeForce RTX 5080" -> "RTX 5080", for the narrow cards.
fn short_gpu_name(name: &str) -> String {
    let name = name.replace("(TM)", "").replace("(R)", "");
    let trimmed = ["NVIDIA GeForce ", "NVIDIA ", "AMD ", "Intel "]
        .iter()
        .find_map(|prefix| name.strip_prefix(prefix))
        .unwrap_or(&name);
    trimmed.trim().to_string()
}

/// A grid of small labelled graphs (cores, GPU engines). Returns the index
/// of a clicked cell.
/// One cell of `mini_graphs`.
struct Mini<'a> {
    label: String,
    ring: &'a Ring,
    value: f32,
    /// Small text inside the graph, e.g. the busiest process on a core.
    caption: Option<String>,
}

fn mini_graphs(
    ui: &mut Ui,
    items: &[Mini<'_>],
    color: egui::Color32,
    selected: Option<usize>,
) -> Option<usize> {
    let n = items.len();
    let columns = match n {
        0..=4 => n.max(1),
        5..=16 => 4,
        17..=64 => 8,
        _ => 16,
    };
    let gap = 6.0;
    let width = ui.available_width();
    let cell_w = ((width - gap * (columns - 1) as f32) / columns as f32).max(40.0);
    let mut clicked = None;
    for (row, chunk) in items.chunks(columns).enumerate() {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            for (i, item) in chunk.iter().enumerate() {
                let Mini {
                    label,
                    ring,
                    value,
                    caption,
                } = item;
                let index = row * columns + i;
                let (rect, response) = ui.allocate_exact_size(vec2(cell_w, 66.0), Sense::click());
                let label_rect = Rect::from_min_size(rect.min, vec2(rect.width(), 14.0));
                let painter = ui.painter_at(label_rect);
                painter.text(
                    label_rect.left_center(),
                    Align2::LEFT_CENTER,
                    label,
                    FontId::proportional(theme::CAPTION),
                    theme::TEXT_DIM,
                );
                painter.text(
                    label_rect.right_center(),
                    Align2::RIGHT_CENTER,
                    format!("{value:.0}%"),
                    FontId::proportional(theme::CAPTION),
                    if *value >= 90.0 {
                        theme::ELEVATED_TEXT
                    } else {
                        theme::TEXT_WEAK
                    },
                );
                let graph_rect = Rect::from_min_max(rect.min + vec2(0.0, 15.0), rect.max);
                Graph::new(Some(100.0))
                    .series(ring, color, true)
                    .no_grid()
                    .paint(ui, graph_rect);
                if let Some(caption) = caption {
                    ui.painter_at(graph_rect.shrink(3.0)).text(
                        graph_rect.left_top() + vec2(4.0, 3.0),
                        Align2::LEFT_TOP,
                        caption,
                        FontId::proportional(theme::CAPTION),
                        theme::TEXT_WEAK,
                    );
                }
                if selected == Some(index) || response.hovered() {
                    let stroke = if selected == Some(index) {
                        theme::ACCENT
                    } else {
                        theme::TEXT_WEAK
                    };
                    ui.painter().rect_stroke(
                        graph_rect,
                        2.0,
                        egui::Stroke::new(1.0, stroke),
                        egui::StrokeKind::Inside,
                    );
                }
                if response.clicked() {
                    clicked = Some(index);
                }
            }
        });
        ui.add_space(gap - ui.spacing().item_spacing.y);
    }
    clicked
}

fn pct(part: u64, whole: u64) -> f64 {
    if whole == 0 {
        0.0
    } else {
        part as f64 * 100.0 / whole as f64
    }
}

fn title(ui: &mut Ui, name: &str, right: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(name).font(theme::brand(theme::TITLE)));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(
                RichText::new(right)
                    .size(theme::BODY)
                    .color(theme::TEXT_WEAK),
            );
        });
    });
    ui.add_space(4.0);
}

/// A full-width graph with its scale and time span written in the corners.
fn big_graph(
    ui: &mut Ui,
    graph: Graph<'_>,
    height: f32,
    ring: &Ring,
    snap: &Snapshot,
    scale: impl Fn(f32) -> String,
) {
    let width = ui.available_width();
    let (response, max) = graph.show(ui, vec2(width, height));
    let rect = response.rect;
    let interval = (snap.interval.as_secs_f32() * 4.0).round().max(1.0) / 4.0;
    let span = interval * crate::graph::visible_samples(ring, rect.width()) as f32;
    let span = if span >= 120.0 {
        format!("{:.0} min", span / 60.0)
    } else {
        format!("{span:.0} s")
    };
    let small = FontId::proportional(theme::CAPTION);
    ui.painter().text(
        rect.left_top() + vec2(6.0, 4.0),
        Align2::LEFT_TOP,
        scale(max),
        small.clone(),
        theme::TEXT_DIM,
    );
    ui.painter().text(
        rect.left_bottom() + vec2(6.0, -4.0),
        Align2::LEFT_BOTTOM,
        span,
        small,
        theme::TEXT_DIM,
    );
}

fn legend(ui: &mut Ui, items: &[(egui::Color32, &str)]) {
    ui.horizontal(|ui| {
        for (color, label) in items {
            let (rect, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
            ui.painter().rect_filled(rect, 2.0, *color);
            ui.label(RichText::new(*label).small().color(theme::TEXT_WEAK));
            ui.add_space(8.0);
        }
    });
}

fn stats(ui: &mut Ui, items: &[(&str, String)]) {
    for chunk in items.chunks(4) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 18.0;
            for (caption, value) in chunk {
                widgets::stat(
                    ui,
                    160.0,
                    caption,
                    RichText::new(value).size(theme::SUBTITLE),
                );
            }
        });
    }
}

/// The few processes responsible for most of a metric right now.
fn top(
    ui: &mut Ui,
    snap: &Snapshot,
    label: &str,
    metric: impl Fn(&ProcessInfo) -> f64,
    show: impl Fn(f64) -> String,
    out: &mut Vec<UiCmd>,
) {
    let mut procs: Vec<(&ProcessInfo, f64)> = snap
        .processes
        .iter()
        .filter(|p| p.state != ProcState::Exited && p.pid != 0)
        .map(|p| (p, metric(p)))
        .filter(|(_, v)| *v > 0.0)
        .collect();
    procs.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    ui.add_space(10.0);
    widgets::section(ui, label);
    let shown = procs.len().min(TOP_ROWS);
    for (i, (p, v)) in procs.into_iter().take(TOP_ROWS).enumerate() {
        widgets::row(ui, i, |ui| {
            widgets::cell(ui, 260.0, Align::Min, |ui| {
                if widgets::link(ui, &p.name)
                    .on_hover_text("Show in the process list")
                    .clicked()
                {
                    out.push(UiCmd::GoTo(p.key));
                }
            });
            let pid = if p.is_pseudo() {
                String::new()
            } else {
                p.pid.to_string()
            };
            widgets::text(
                ui,
                70.0,
                Align::Max,
                RichText::new(pid).color(theme::TEXT_DIM),
            );
            widgets::text(ui, 100.0, Align::Max, show(v));
        });
    }
    widgets::blank_rows(ui, shown, TOP_ROWS, 430.0);
}

const TOP_ROWS: usize = 8;
const DISK_PROCESS_ROWS: usize = 12;
const DISK_FILE_ROWS: usize = 20;
const CORE_ROWS: usize = 12;

fn zombies_section(ui: &mut Ui, snap: &Snapshot, zombies: &ZombieView, out: &mut Vec<UiCmd>) {
    let s = &snap.system;
    // Windows: the idle process has a process object too. Linux lists its
    // zombies as processes, ended and waiting for their parent.
    let estimate = if cfg!(windows) {
        s.process_objects.saturating_sub(s.processes + 1)
    } else {
        snap.processes
            .iter()
            .filter(|p| p.ending && p.state != ProcState::Exited)
            .count() as u32
    };
    let (what, scanning, find) = if cfg!(windows) {
        (
            "exited processes the kernel cannot free because someone still holds a handle to them",
            "Scanning every handle in the system…",
            "Find who holds them",
        )
    } else {
        (
            "ended processes whose parent has not collected them; each keeps only its PID",
            "Reading every process…",
            "Find their parents",
        )
    };
    ui.add_space(14.0);
    widgets::section(ui, "Zombie processes");
    ui.horizontal(|ui| {
        let color = if estimate > 1000 {
            theme::ELEVATED_TEXT
        } else {
            theme::TEXT
        };
        ui.label(widgets::strong(fmt::count(estimate as u64), theme::TITLE).color(color));
        ui.label(RichText::new(what).small().color(theme::TEXT_WEAK));
    });
    ui.horizontal(|ui| {
        if zombies.pending {
            ui.spinner();
            ui.label(RichText::new(scanning).color(theme::TEXT_WEAK));
        } else if ui.button(find).clicked() {
            out.push(UiCmd::FindZombies);
        }
        if let Some(r) = &zombies.report {
            ui.label(
                RichText::new(format!(
                    "last scan: {} zombies, {}",
                    fmt::count(r.zombies as u64),
                    fmt::millis(r.took)
                ))
                .small()
                .color(theme::TEXT_DIM),
            );
        }
    });
    let Some(report) = &zombies.report else {
        return;
    };
    let columns = [
        (
            if cfg!(windows) { "Holder" } else { "Parent" },
            260.0,
            Align::Min,
        ),
        ("PID", 70.0, Align::Max),
        ("Zombies", 90.0, Align::Max),
        // A parent holds its zombies by not collecting them, not by handles.
        (
            if cfg!(windows) { "Handles" } else { "" },
            100.0,
            Align::Max,
        ),
    ];
    widgets::header(ui, &columns);
    for (i, h) in report.holders.iter().take(12).enumerate() {
        widgets::row(ui, i, |ui| {
            widgets::cell(ui, columns[0].1, Align::Min, |ui| {
                match snap
                    .processes
                    .iter()
                    .find(|p| p.pid == h.pid && p.state != ProcState::Exited)
                {
                    Some(p) => {
                        if widgets::link(ui, &p.name)
                            .on_hover_text("Show in the process list")
                            .clicked()
                        {
                            out.push(UiCmd::GoTo(p.key));
                        }
                    }
                    None => {
                        ui.label(RichText::new("<exited>").color(theme::TEXT_DIM));
                    }
                }
            });
            widgets::text(
                ui,
                columns[1].1,
                Align::Max,
                RichText::new(h.pid.to_string()).color(theme::TEXT_DIM),
            );
            widgets::text(ui, columns[2].1, Align::Max, fmt::count(h.zombies as u64));
            if cfg!(windows) {
                widgets::text(
                    ui,
                    columns[3].1,
                    Align::Max,
                    RichText::new(fmt::count(h.handles as u64)).color(theme::TEXT_WEAK),
                );
            }
        });
    }
}

/// Two columns side by side when there is room (numbers left, the processes
/// behind them right), stacked when there is not.
fn band(ui: &mut Ui, left: impl FnOnce(&mut Ui), right: impl FnOnce(&mut Ui)) {
    const LEFT: f32 = 712.0;
    if ui.available_width() < LEFT + 460.0 {
        left(ui);
        right(ui);
        return;
    }
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(vec2(LEFT, 0.0), Layout::top_down(Align::Min), |ui| {
            ui.set_width(LEFT);
            left(ui);
        });
        ui.add_space(24.0);
        ui.vertical(|ui| right(ui));
    });
}

fn memory(ui: &mut Ui, snap: &Snapshot, zombies: &ZombieView, out: &mut Vec<UiCmd>) {
    let h = &snap.history;
    let m = &snap.system.memory;
    title(ui, "Memory", &fmt::bytes(m.total));
    big_graph(
        ui,
        Graph::new(Some(m.total.max(1) as f32)).series(&h.memory_used, theme::MEMORY, true),
        160.0,
        &h.memory_used,
        snap,
        |v| fmt::bytes(v as u64),
    );
    ui.add_space(8.0);

    if let Some(lists) = m.lists {
        ui.label(
            RichText::new("Composition")
                .size(theme::META)
                .color(theme::TEXT_WEAK),
        );
        let in_use = m.used().saturating_sub(lists.modified);
        let free = lists.free + lists.zeroed;
        let parts = [
            (in_use, theme::MEMORY, "In use"),
            (lists.modified, theme::COMMIT, "Modified"),
            (lists.standby, theme::MEMORY_STANDBY, "Standby"),
            (free, theme::BORDER_STRONG, "Free"),
        ];
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 22.0), Sense::hover());
        let total: u64 = parts.iter().map(|p| p.0).sum::<u64>().max(1);
        let mut x = rect.left();
        for (value, color, _) in parts {
            let w = rect.width() * value as f32 / total as f32;
            ui.painter().rect_filled(
                Rect::from_min_size(egui::pos2(x, rect.top()), vec2(w, rect.height())),
                0.0,
                color,
            );
            x += w;
        }
        ui.horizontal(|ui| {
            for (value, color, label) in parts {
                let (r, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
                ui.painter().rect_filled(r, 2.0, color);
                ui.label(
                    RichText::new(format!("{label} {}", fmt::bytes(value)))
                        .small()
                        .color(theme::TEXT_WEAK),
                );
                ui.add_space(8.0);
            }
        });
        ui.add_space(8.0);
    }

    ui.label(
        RichText::new("Commit charge")
            .small()
            .color(theme::TEXT_WEAK),
    );
    big_graph(
        ui,
        Graph::new(Some(m.commit_limit.max(1) as f32)).series(&h.commit, theme::COMMIT, true),
        90.0,
        &h.commit,
        snap,
        |v| fmt::bytes(v as u64),
    );
    ui.add_space(10.0);

    let compressed = snap
        .processes
        .iter()
        .find(|p| p.state != ProcState::Exited && &*p.name == "Memory Compression")
        .map_or(0, |p| p.working_set);
    let cached = m.lists.map_or(m.system_cache, |l| l.standby + l.modified);
    stats(
        ui,
        &[
            (
                "In use",
                format!("{} ({:.0} %)", fmt::bytes(m.used()), pct(m.used(), m.total)),
            ),
            ("Available", fmt::bytes(m.available)),
            (
                "Committed",
                format!("{} / {}", fmt::bytes(m.commit), fmt::bytes(m.commit_limit)),
            ),
            ("Cached", fmt::bytes(cached)),
            ("Compressed", fmt::bytes(compressed)),
            ("Paged pool", fmt::bytes(m.paged_pool)),
            ("Non-paged pool", fmt::bytes(m.nonpaged_pool)),
            ("Peak commit", fmt::bytes(m.commit_peak)),
        ],
    );
    top(
        ui,
        snap,
        "Top memory (private working set)",
        |p| p.private_working_set as f64,
        |v| fmt::bytes(v as u64),
        out,
    );
    top(
        ui,
        snap,
        "Top private bytes (committed)",
        |p| p.private_bytes as f64,
        |v| fmt::bytes(v as u64),
        out,
    );
    zombies_section(ui, snap, zombies, out);
}

fn io(ui: &mut Ui, snap: &Snapshot, out: &mut Vec<UiCmd>) {
    let h = &snap.history;
    let s = &snap.system;
    title(ui, "Process I/O", "files, devices, pipes and network");
    big_graph(
        ui,
        Graph::new(None)
            .series(&h.io_read, theme::DISK_READ, true)
            .series(&h.io_write, theme::DISK_WRITE, true),
        200.0,
        &h.io_read,
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
            ("Read", fmt::rate_always(s.io_read_rate)),
            ("Write", fmt::rate_always(s.io_write_rate)),
            ("Other", fmt::rate_always(s.io_other_rate)),
        ],
    );
    top(
        ui,
        snap,
        "Top I/O",
        |p| p.io_total_rate(),
        fmt::rate_always,
        out,
    );
}

fn network(ui: &mut Ui, snap: &Snapshot, view: &mut PerfView, out: &mut Vec<UiCmd>) {
    let h = &snap.history;
    let net = &snap.system.net;
    let names: Vec<&str> = net.adapters.iter().map(|a| a.name.as_str()).collect();
    title(ui, "Network", &names.join(", "));
    big_graph(
        ui,
        Graph::new(None)
            .series(&h.net_rx, theme::NET_RX, true)
            .series(&h.net_tx, theme::NET_TX, true),
        200.0,
        &h.net_rx,
        snap,
        |v| format!("{}/s", fmt::bytes(v as u64)),
    );
    ui.horizontal(|ui| {
        legend(ui, &[(theme::NET_RX, "Receive"), (theme::NET_TX, "Send")]);
        if ui
            .button("Who is using it: the Network page (Ctrl+8)")
            .clicked()
        {
            out.push(UiCmd::ShowNetwork);
        }
    });
    ui.add_space(10.0);
    stats(
        ui,
        &[
            ("Receive", fmt::rate_always(net.rx_rate)),
            ("Send", fmt::rate_always(net.tx_rate)),
        ],
    );
    ui.add_space(10.0);
    let columns = [
        ("Adapter", 200.0, Align::Min),
        ("Link", 90.0, Align::Max),
        ("Receive", 100.0, Align::Max),
        ("Send", 100.0, Align::Max),
        ("Received", 100.0, Align::Max),
        ("Sent", 100.0, Align::Max),
    ];
    widgets::header(ui, &columns);
    for (i, a) in net.adapters.iter().enumerate() {
        widgets::row(ui, i, |ui| {
            widgets::cell(ui, columns[0].1, Align::Min, |ui| {
                if widgets::link(ui, &a.name)
                    .on_hover_text(&a.description)
                    .clicked()
                {
                    *view = PerfView::Adapter(a.luid);
                }
            });
            widgets::text(ui, columns[1].1, Align::Max, link_speed(a.link_speed));
            widgets::text(ui, columns[2].1, Align::Max, fmt::rate_always(a.rx_rate));
            widgets::text(ui, columns[3].1, Align::Max, fmt::rate_always(a.tx_rate));
            widgets::text(
                ui,
                columns[4].1,
                Align::Max,
                RichText::new(fmt::bytes(a.rx_total)).color(theme::TEXT_WEAK),
            );
            widgets::text(
                ui,
                columns[5].1,
                Align::Max,
                RichText::new(fmt::bytes(a.tx_total)).color(theme::TEXT_WEAK),
            );
        });
    }
    match &snap.system.net_trace_error {
        Some(e) => {
            ui.add_space(10.0);
            ui.label(
                RichText::new(format!("Per-process network traffic is unavailable: {e}"))
                    .color(theme::ELEVATED_TEXT),
            );
        }
        None => top(
            ui,
            snap,
            "Top network (sent + received)",
            |p| p.net_total_rate(),
            fmt::rate_always,
            out,
        ),
    }
}

fn link_speed(bits: u64) -> String {
    match bits {
        0 => "—".into(),
        b if b >= 1_000_000_000 => format!("{} Gbps", b / 1_000_000_000),
        b => format!("{} Mbps", b / 1_000_000),
    }
}

fn gpu(ui: &mut Ui, snap: &Snapshot, index: usize, out: &mut Vec<UiCmd>) {
    let (Some(g), Some(gh)) = (snap.system.gpus.get(index), snap.history.gpus.get(index)) else {
        ui.label(RichText::new("No such GPU").color(theme::TEXT_WEAK));
        return;
    };
    title(ui, &format!("GPU {index}"), &g.name);
    big_graph(
        ui,
        Graph::new(Some(100.0)).series(&gh.usage, theme::GPU, true),
        170.0,
        &gh.usage,
        snap,
        |m| format!("{m:.0} %"),
    );
    ui.label(
        RichText::new("Busiest engine")
            .small()
            .color(theme::TEXT_WEAK),
    );
    ui.add_space(8.0);
    let items: Vec<Mini<'_>> = g
        .engines
        .iter()
        .zip(&gh.engines)
        .map(|(e, ring)| Mini {
            label: e.name.to_string(),
            ring,
            value: e.usage,
            caption: None,
        })
        .collect();

    mini_graphs(ui, &items, theme::GPU, None);
    ui.add_space(8.0);
    if g.dedicated_total > 0 {
        ui.label(
            RichText::new("Dedicated memory")
                .small()
                .color(theme::TEXT_WEAK),
        );
        big_graph(
            ui,
            Graph::new(Some(g.dedicated_total as f32)).series(&gh.dedicated, theme::MEMORY, true),
            80.0,
            &gh.dedicated,
            snap,
            |v| fmt::bytes(v as u64),
        );
    }
    ui.add_space(10.0);
    stats(
        ui,
        &[
            ("Utilization", format!("{:.1} %", g.usage)),
            (
                "Dedicated memory",
                format!(
                    "{} / {}",
                    fmt::bytes(g.dedicated_used),
                    fmt::bytes(g.dedicated_total)
                ),
            ),
            (
                "Shared memory",
                format!(
                    "{} / {}",
                    fmt::bytes(g.shared_used),
                    fmt::bytes(g.shared_total)
                ),
            ),
        ],
    );
    top(
        ui,
        snap,
        "Top GPU (busiest engine, all GPUs)",
        |p| p.gpu as f64,
        |v| format!("{v:.1} %"),
        out,
    );
}

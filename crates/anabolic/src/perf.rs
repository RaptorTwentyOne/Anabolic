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
                    nav(ui, view, snap, hw);
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

fn nav(ui: &mut Ui, view: &mut PerfView, snap: &Snapshot, hw: &HardwareInfo) {
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
    // The idle process has a process object too.
    let estimate = s.process_objects.saturating_sub(s.processes + 1);
    ui.add_space(14.0);
    widgets::section(ui, "Zombie processes");
    ui.horizontal(|ui| {
        let color = if estimate > 1000 { theme::ELEVATED_TEXT } else { theme::TEXT };
        ui.label(widgets::strong(fmt::count(estimate as u64), theme::TITLE).color(color));
        ui.label(
            RichText::new("exited processes the kernel cannot free because someone still holds a handle to them")
                .small()
                .color(theme::TEXT_WEAK),
        );
    });
    ui.horizontal(|ui| {
        if zombies.pending {
            ui.spinner();
            ui.label(RichText::new("Scanning every handle in the system…").color(theme::TEXT_WEAK));
        } else if ui.button("Find who holds them").clicked() {
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
        ("Holder", 260.0, Align::Min),
        ("PID", 70.0, Align::Max),
        ("Zombies", 90.0, Align::Max),
        ("Handles", 100.0, Align::Max),
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
            widgets::text(
                ui,
                columns[3].1,
                Align::Max,
                RichText::new(fmt::count(h.handles as u64)).color(theme::TEXT_WEAK),
            );
        });
    }
}

fn cpu(
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
    band(
        ui,
        |ui| {
            stats(
                ui,
                &[
                    ("Utilization", format!("{:.1} %", s.cpu.total)),
                    ("Speed", ghz(s.cpu.mhz)),
                    ("Kernel", format!("{:.1} %", s.cpu.kernel)),
                    ("DPC", format!("{:.2} %", s.cpu.dpc)),
                    ("Interrupts", format!("{:.2} %", s.cpu.interrupt)),
                    ("Processes", fmt::count(s.processes as u64)),
                    ("Threads", fmt::count(s.threads as u64)),
                    ("Handles", fmt::count(s.handles as u64)),
                    ("Up time", fmt::duration(s.uptime)),
                    (
                        "Context switches/s",
                        fmt::count(s.cpu.context_switch_rate as u64),
                    ),
                    ("Interrupts/s", fmt::count(s.cpu.interrupt_rate as u64)),
                ],
            )
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

fn disk(ui: &mut Ui, snap: &Snapshot, view: &mut PerfView, out: &mut Vec<UiCmd>) {
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
            RichText::new("Open a disk to see which processes and files use it.")
                .small()
                .color(theme::TEXT_DIM),
        );
    }
    if snap.info_level != anabolic_core::InfoLevel::Full {
        ui.label(
            RichText::new("Per-process disk counters need administrator rights.")
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

fn disk_kind(d: &anabolic_core::disk::DiskStats) -> String {
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

fn physical_disk(ui: &mut Ui, snap: &Snapshot, number: u32, out: &mut Vec<UiCmd>) {
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
fn process_link(ui: &mut Ui, snap: &Snapshot, pid: u32, name: &str, out: &mut Vec<UiCmd>) {
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

fn disk_use_cells(ui: &mut Ui, io: &DiskUse, widths: [f32; 3]) {
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
fn disk_users(ui: &mut Ui, snap: &Snapshot, number: u32, out: &mut Vec<UiCmd>) {
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
    legend(ui, &[(theme::NET_RX, "Receive"), (theme::NET_TX, "Send")]);
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

/// "0-7, 16-23" for a set of processor indices.
fn cpu_ranges(cpus: &[usize]) -> String {
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

fn cache_text(topology: &Topology, index: Option<usize>, name: &str, cpu: usize) -> Option<String> {
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
fn core_detail(
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
            ("DPC", format!("{:.2} %", times.dpc)),
            ("Interrupt time", format!("{:.2} %", times.interrupt)),
            ("Interrupts/s", fmt::count(times.interrupt_rate as u64)),
            (
                "Context switches/s",
                fmt::count(times.context_switch_rate as u64),
            ),
            ("DPCs/s", fmt::count(times.dpc_rate as u64)),
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

fn share_bar(ui: &mut Ui, share: f32, idle: bool) {
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

fn adapter(ui: &mut Ui, snap: &Snapshot, luid: u64) {
    let Some(a) = snap.system.net.adapters.iter().find(|a| a.luid == luid) else {
        ui.label(RichText::new("This adapter is down or gone.").color(theme::TEXT_WEAK));
        return;
    };
    title(ui, &a.name, &a.description);
    if let Some(ah) = snap.history.adapter(luid) {
        big_graph(
            ui,
            Graph::new(None).series(&ah.rx, theme::NET_RX, true).series(
                &ah.tx,
                theme::NET_TX,
                true,
            ),
            200.0,
            &ah.rx,
            snap,
            |v| format!("{}/s", fmt::bytes(v as u64)),
        );
        legend(ui, &[(theme::NET_RX, "Receive"), (theme::NET_TX, "Send")]);
    }
    ui.add_space(10.0);
    let utilization = if a.link_speed > 0 {
        (a.rx_rate + a.tx_rate) * 8.0 * 100.0 / a.link_speed as f64
    } else {
        0.0
    };
    stats(
        ui,
        &[
            ("Receive", fmt::rate_always(a.rx_rate)),
            ("Send", fmt::rate_always(a.tx_rate)),
            ("Link speed", link_speed(a.link_speed)),
            ("Link use", format!("{utilization:.2} %")),
            ("Received", fmt::bytes(a.rx_total)),
            ("Sent", fmt::bytes(a.tx_total)),
        ],
    );
}

/// Key/value rows with fixed columns.
fn facts(ui: &mut Ui, items: &[(&str, String)]) {
    for (i, (key, value)) in items.iter().enumerate() {
        widgets::row(ui, i, |ui| {
            widgets::text(
                ui,
                190.0,
                Align::Min,
                RichText::new(*key).color(theme::TEXT_WEAK),
            );
            widgets::text(ui, 520.0, Align::Min, value.clone());
        });
    }
}

fn section(ui: &mut Ui, name: &str) {
    ui.add_space(14.0);
    ui.label(widgets::strong(name, theme::SUBTITLE));
    ui.add_space(4.0);
}

fn cpu_info(ui: &mut Ui, snap: &Snapshot, hw: &HardwareInfo) {
    let c = &hw.cpu;
    title(ui, "Processor info", &c.brand);
    let smt = if c.threads > c.cores {
        "SMT on"
    } else {
        "SMT off"
    };
    facts(
        ui,
        &[
            ("Name", c.brand.clone()),
            ("Vendor", c.vendor.clone()),
            (
                "Family / model / stepping",
                format!(
                    "{} ({:X}h) / {} ({:X}h) / {}",
                    c.family, c.family, c.model, c.model, c.stepping
                ),
            ),
            (
                "Socket",
                format!(
                    "{}  ·  {} socket(s)",
                    if c.socket.is_empty() { "?" } else { &c.socket },
                    c.sockets.max(1)
                ),
            ),
            (
                "Cores / threads",
                format!("{} / {}  ({smt})", c.cores, c.threads),
            ),
            ("NUMA nodes", c.numa_nodes.to_string()),
            ("Base clock", ghz(c.base_mhz as f32)),
            ("Max clock (firmware)", ghz(c.max_mhz as f32)),
            (
                "Effective clock now",
                format!("{}  (average of active cores)", ghz(snap.system.cpu.mhz)),
            ),
            (
                "Bus clock",
                if c.bus_mhz > 0 {
                    format!("{} MHz", c.bus_mhz)
                } else {
                    "—".into()
                },
            ),
            (
                "Voltage (firmware)",
                c.voltage.map_or("—".into(), |v| format!("{v:.2} V")),
            ),
            (
                "Microcode",
                c.microcode.clone().unwrap_or_else(|| "—".into()),
            ),
            (
                "Virtualization",
                if c.virtualization_enabled {
                    "Enabled in firmware".into()
                } else {
                    "Disabled".into()
                },
            ),
            (
                "Hypervisor",
                if c.hypervisor_present {
                    "Windows runs on a hypervisor (VBS / Hyper-V)".into()
                } else {
                    "None".into()
                },
            ),
        ],
    );

    section(ui, "Caches");
    let caches: Vec<(&str, String)> = c
        .caches
        .iter()
        .map(|(name, size, count)| {
            (
                name.as_str(),
                format!("{} × {}", count, fmt::bytes(*size as u64)),
            )
        })
        .collect();
    facts(ui, &caches);

    section(ui, "Instruction set extensions");
    ui.horizontal_wrapped(|ui| {
        for f in &c.features {
            egui::Frame::new()
                .fill(theme::RAISED)
                .stroke(egui::Stroke::new(1.0, theme::BORDER))
                .corner_radius(3)
                .inner_margin(egui::Margin::symmetric(6, 2))
                .show(ui, |ui| ui.label(RichText::new(*f).small()));
        }
    });

    section(ui, "Live clocks");
    let cores = &snap.system.cores;
    let max = cores
        .iter()
        .map(|c| c.mhz)
        .fold(c.max_mhz as f32, f32::max)
        .max(1.0);
    for (row, chunk) in cores.chunks(8).enumerate() {
        ui.horizontal(|ui| {
            for (i, core) in chunk.iter().enumerate() {
                let cpu = row * 8 + i;
                let domain = hw
                    .topology
                    .l3_domain(cpu)
                    .map_or(String::new(), |d| format!("CCD {d} "));
                widgets::cell(ui, 150.0, Align::Min, |ui| {
                    ui.label(
                        RichText::new(format!("{domain}CPU {cpu}"))
                            .small()
                            .color(theme::TEXT_DIM),
                    );
                    let color = if core.mhz <= 0.0 {
                        theme::TEXT_DIM
                    } else {
                        // Brighter for the faster cores, in the CPU hue.
                        theme::TEXT_WEAK.lerp_to_gamma(theme::CPU, core.mhz / max)
                    };
                    ui.label(RichText::new(ghz(core.mhz)).color(color));
                });
            }
        });
    }

    section(ui, "Board & firmware");
    let b = &hw.board;
    facts(
        ui,
        &[
            ("Motherboard", format!("{} {}", b.manufacturer, b.product)),
            (
                "BIOS",
                format!("{} {}  ·  {}", b.bios_vendor, b.bios_version, b.bios_date),
            ),
        ],
    );
}

fn memory_info(ui: &mut Ui, snap: &Snapshot, hw: &HardwareInfo) {
    let m = &hw.memory;
    let usable = snap.system.memory.total;
    title(
        ui,
        "Memory modules",
        &format!("{} installed", fmt::bytes(m.installed)),
    );
    let rated = m.modules.iter().map(|x| x.speed).max().unwrap_or(0);
    let configured = m
        .modules
        .iter()
        .map(|x| x.configured_speed)
        .filter(|&v| v > 0)
        .min()
        .unwrap_or(0);
    let kind = m.modules.first().map_or("", |x| x.kind);
    facts(
        ui,
        &[
            ("Installed", fmt::bytes(m.installed)),
            ("Usable by Windows", fmt::bytes(usable)),
            (
                "Hardware reserved",
                fmt::bytes(m.installed.saturating_sub(usable)),
            ),
            ("Slots used", format!("{} of {}", m.modules.len(), m.slots)),
            ("Type", kind.to_string()),
            (
                "Running at",
                if configured > 0 {
                    format!("{configured} MT/s")
                } else {
                    "—".into()
                },
            ),
            (
                "Modules rated",
                if rated > 0 {
                    format!("{rated} MT/s (SPD/JEDEC)")
                } else {
                    "—".into()
                },
            ),
            (
                "Maximum capacity",
                if m.max_capacity > 0 {
                    fmt::bytes(m.max_capacity)
                } else {
                    "—".into()
                },
            ),
            ("Error correction", m.error_correction.to_string()),
        ],
    );
    if configured > 0 && rated > 0 && configured < rated {
        ui.add_space(6.0);
        ui.label(
            RichText::new(format!(
                "The memory runs at {configured} MT/s, below the {rated} MT/s the modules report. Check the memory \
                 profile (XMP / EXPO) in the BIOS; boards with every slot filled often need a lower speed to be stable."
            ))
            .color(theme::ELEVATED_TEXT),
        );
    }

    section(ui, "Modules");
    let columns = [
        ("Slot", 190.0, Align::Min),
        ("Size", 70.0, Align::Max),
        ("Type", 90.0, Align::Min),
        ("Rated", 80.0, Align::Max),
        ("Running", 80.0, Align::Max),
        ("Rank", 50.0, Align::Max),
        ("Voltage", 70.0, Align::Max),
        ("Manufacturer", 120.0, Align::Min),
        ("Part number", 200.0, Align::Min),
    ];
    widgets::header(ui, &columns);
    for (i, x) in m.modules.iter().enumerate() {
        widgets::row(ui, i, |ui| {
            let slot = if x.bank.is_empty() {
                x.locator.clone()
            } else {
                format!("{}  ·  {}", x.locator, x.bank)
            };
            widgets::text(ui, columns[0].1, Align::Min, slot);
            widgets::text(ui, columns[1].1, Align::Max, fmt::bytes(x.size));
            widgets::text(
                ui,
                columns[2].1,
                Align::Min,
                format!("{} {}", x.kind, x.form_factor),
            );
            widgets::text(ui, columns[3].1, Align::Max, format!("{}", x.speed));
            widgets::text(
                ui,
                columns[4].1,
                Align::Max,
                format!("{}", x.configured_speed),
            );
            widgets::text(
                ui,
                columns[5].1,
                Align::Max,
                if x.rank > 0 {
                    x.rank.to_string()
                } else {
                    String::new()
                },
            );
            widgets::text(
                ui,
                columns[6].1,
                Align::Max,
                if x.voltage_mv > 0 {
                    format!("{:.2} V", x.voltage_mv as f32 / 1000.0)
                } else {
                    String::new()
                },
            );
            widgets::text(ui, columns[7].1, Align::Min, x.manufacturer.clone());
            widgets::text(
                ui,
                columns[8].1,
                Align::Min,
                RichText::new(x.part_number.clone()).monospace(),
            );
        });
    }

    section(ui, "Page files");
    let columns = [
        ("File", 260.0, Align::Min),
        ("Size", 100.0, Align::Max),
        ("In use", 100.0, Align::Max),
        ("Peak", 100.0, Align::Max),
    ];
    widgets::header(ui, &columns);
    for (i, pf) in snap.system.page_files.iter().enumerate() {
        widgets::row(ui, i, |ui| {
            widgets::text(ui, columns[0].1, Align::Min, pf.path.clone());
            widgets::text(ui, columns[1].1, Align::Max, fmt::bytes(pf.size));
            widgets::text(ui, columns[2].1, Align::Max, fmt::bytes(pf.used));
            widgets::text(
                ui,
                columns[3].1,
                Align::Max,
                RichText::new(fmt::bytes(pf.peak)).color(theme::TEXT_WEAK),
            );
        });
    }
    if snap.system.page_files.is_empty() {
        ui.label(RichText::new("No page file.").color(theme::TEXT_DIM));
    }
}

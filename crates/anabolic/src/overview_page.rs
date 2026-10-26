// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! The Overview page: the machine at a glance. A tile per resource, each
//! total with the app behind most of it; what needs attention, with what
//! can be done about it; the busiest apps; and the quick ways in: running a
//! program, finding a handle, freeing a port.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use anabolic_core::growth::Leak;
use anabolic_core::hwinfo::HardwareInfo;
use anabolic_core::sensors::{Heat, SensorKind};
use anabolic_core::{InfoLevel, NetActivity, ProcKey, ProcState, Snapshot};
use eframe::egui::{
    self, Align, CursorIcon, Frame, Label, Layout, Margin, RichText, Sense, Stroke, StrokeKind, Ui,
    UiBuilder, vec2,
};

use crate::graph::Graph;
use crate::icons::IconCache;
use crate::perf::PerfView;
use crate::proc_table::UiCmd;
use crate::runaway::Runaway;
use crate::settings::Tab;
use crate::tree::Details;
use crate::{fmt, theme, widgets};

const GAP: f32 = 12.0;
/// A tile's height without its graph.
const TILE_TEXT: f32 = 92.0;
/// A row of a card's list.
const ROW_HEIGHT: f32 = 22.0;
const APP_ROWS: usize = 10;
const RECENT_ROWS: usize = 8;
const PORT_ROWS: usize = 6;
/// "Recently started" goes this far back.
const RECENT_MINUTES: i64 = 10;
/// A volume with less free than this, and less than a tenth of it, is full.
const LOW_SPACE: u64 = 10 << 30;

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum AppSort {
    #[default]
    Cpu,
    Memory,
    Disk,
    Network,
    Gpu,
}

#[derive(Default)]
pub struct OverviewPage {
    pub sort: AppSort,
}

/// What the page reads besides the snapshot.
pub struct Inputs<'a> {
    pub snap: &'a Snapshot,
    pub details: &'a Details,
    pub cmdlines: &'a HashMap<ProcKey, Arc<str>>,
    pub hw: &'a HardwareInfo,
    pub cpu_name: &'a str,
    pub runaways: &'a [Runaway],
    pub leaks: &'a [Leak],
    /// RAM or commit nearly full: which, and how full.
    pub memory_alert: Option<(&'static str, f64)>,
    pub elevated: bool,
    pub relaunching: bool,
    pub watched_ports: &'a [crate::settings::PortWatch],
    /// For a process' menu: what is blocked from the network, whether
    /// sign-in secrets may be dumped, the exits awaited.
    pub blocked: &'a [String],
    pub secrets: bool,
    pub exit_watched: &'a [ProcKey],
}

/// One executable's live processes, summed.
struct App {
    name: Arc<str>,
    /// Its biggest process: whose icon it wears.
    key: ProcKey,
    biggest: u64,
    /// Its biggest process' base priority, for a new rule.
    base_priority: i32,
    keys: Vec<ProcKey>,
    count: usize,
    cpu: f32,
    memory: u64,
    disk: f64,
    net: f64,
    gpu: f32,
}

impl App {
    fn value(&self, sort: AppSort) -> f64 {
        match sort {
            AppSort::Cpu => self.cpu as f64,
            AppSort::Memory => self.memory as f64,
            AppSort::Disk => self.disk,
            AppSort::Network => self.net,
            AppSort::Gpu => self.gpu as f64,
        }
    }

    fn label(&self) -> String {
        if self.count > 1 {
            format!("{} ×{}", self.name, self.count)
        } else {
            self.name.to_string()
        }
    }
}

fn apps(snap: &Snapshot) -> Vec<App> {
    let mut by: HashMap<String, App> = HashMap::new();
    for p in snap
        .processes
        .iter()
        .filter(|p| p.state != ProcState::Exited && !p.is_pseudo())
    {
        let a = by.entry(p.name.to_lowercase()).or_insert_with(|| App {
            name: p.name.clone(),
            key: p.key,
            biggest: 0,
            base_priority: p.base_priority,
            keys: Vec::new(),
            count: 0,
            cpu: 0.0,
            memory: 0,
            disk: 0.0,
            net: 0.0,
            gpu: 0.0,
        });
        if p.private_working_set >= a.biggest {
            a.biggest = p.private_working_set;
            a.key = p.key;
            a.base_priority = p.base_priority;
        }
        a.keys.push(p.key);
        a.count += 1;
        a.cpu += p.cpu;
        a.memory += p.private_working_set;
        a.disk += p.disk_total_rate();
        a.net += p.net_total_rate();
        a.gpu += p.gpu;
    }
    by.into_values().collect()
}

/// The app with the most of `sort`, if any has some.
fn top(apps: &[App], sort: AppSort) -> Option<&App> {
    apps.iter()
        .filter(|a| a.value(sort) > 0.0)
        .max_by(|a, b| a.value(sort).total_cmp(&b.value(sort)))
}

/// This computer's name.
fn host() -> &'static str {
    static HOST: OnceLock<String> = OnceLock::new();
    HOST.get_or_init(|| {
        let name = if cfg!(windows) {
            std::env::var("COMPUTERNAME").ok()
        } else {
            std::fs::read_to_string("/etc/hostname")
                .ok()
                .or_else(|| std::env::var("HOSTNAME").ok())
        };
        name.map(|n| n.trim().to_string()).unwrap_or_default()
    })
}

/// "3 d 4 h", "5 h 12 min", "8 min".
fn uptime(d: Duration) -> String {
    let days = d.as_secs() / 86_400;
    if days > 0 {
        format!("{days} d {} h", d.as_secs() / 3600 % 24)
    } else {
        fmt::span(d)
    }
}

pub fn show(
    ui: &mut Ui,
    page: &mut OverviewPage,
    input: &Inputs<'_>,
    icons: &mut IconCache,
    out: &mut Vec<UiCmd>,
) {
    // The tiles take a quarter of the window's height, their graphs the rest.
    let tile_height = (ui.available_height() * 0.25).clamp(150.0, 260.0);
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            heading(ui, input);
            ui.add_space(8.0);
            let apps = apps(input.snap);
            tiles(ui, input, &apps, tile_height, out);
            ui.add_space(GAP);
            let width = ui.available_width();
            if width >= 900.0 {
                let side = (width * 0.36).clamp(320.0, 460.0);
                let main = width - side - GAP;
                ui.horizontal_top(|ui| {
                    ui.spacing_mut().item_spacing.x = GAP;
                    ui.vertical(|ui| {
                        ui.set_width(main);
                        main_column(ui, page, input, &apps, icons, out);
                    });
                    ui.vertical(|ui| {
                        ui.set_width(side);
                        side_column(ui, input, out);
                    });
                });
            } else {
                main_column(ui, page, input, &apps, icons, out);
                ui.add_space(GAP);
                side_column(ui, input, out);
            }
            ui.add_space(GAP);
        });
}

fn heading(ui: &mut Ui, input: &Inputs<'_>) {
    ui.horizontal(|ui| {
        ui.label(RichText::new("Overview").font(theme::brand(theme::TITLE)));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let mut text = format!("up {}", uptime(input.snap.system.uptime));
            if !host().is_empty() {
                text = format!("{}  ·  {text}", host());
            }
            ui.label(
                RichText::new(text)
                    .size(theme::BODY)
                    .color(theme::TEXT_WEAK),
            );
        });
    });
}

/// A titled panel of the page, `right` beside its title.
fn card<R>(
    ui: &mut Ui,
    title: &str,
    right: impl FnOnce(&mut Ui),
    add: impl FnOnce(&mut Ui) -> R,
) -> R {
    Frame::new()
        .fill(theme::STRIPE)
        .stroke(Stroke::new(1.0, theme::BORDER))
        .corner_radius(theme::RADIUS_PANEL)
        .inner_margin(Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(widgets::strong(title, theme::BODY));
                ui.with_layout(Layout::right_to_left(Align::Center), right);
            });
            ui.add_space(6.0);
            add(ui)
        })
        .inner
}

/// A row of a card's list: one strip to click (what it shows) and right
/// click (its menu), its cells laid over it; `add` is told whether the
/// pointer is on it. Odd rows are a step lighter than the card.
///
/// `what` names what the row shows: the rows trade places as the numbers
/// change, and an open menu stays with what it was opened on.
fn row(
    ui: &mut Ui,
    what: impl std::hash::Hash + std::fmt::Debug,
    index: usize,
    add: impl FnOnce(&mut Ui, bool),
) -> egui::Response {
    let (_, rect) = ui.allocate_space(vec2(ui.available_width(), ROW_HEIGHT));
    let response = ui.interact(rect, ui.id().with(("row", what)), Sense::click());
    let hovered = ui.rect_contains_pointer(rect);
    let fill = if hovered {
        Some(theme::HOVER)
    } else if index % 2 == 1 {
        Some(theme::RAISED)
    } else {
        None
    };
    if let Some(fill) = fill {
        ui.painter()
            .rect_filled(rect.expand2(vec2(4.0, 0.0)), 2.0, fill);
    }
    let mut cells = ui.new_child(
        UiBuilder::new()
            .max_rect(rect)
            .layout(Layout::left_to_right(Align::Center)),
    );
    cells.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
    add(&mut cells, hovered);
    response.on_hover_cursor(CursorIcon::PointingHand)
}

/// A row's menu, kept by what it was opened on (`opened`, from the row
/// right clicked this frame) rather than by the row: as the numbers change,
/// rows trade places and drop out of the card, and the menu stays open on
/// what it was opened on for as long as `there` finds it. `add` fills it.
fn held_menu<T: Clone + Send + Sync + 'static>(
    ui: &Ui,
    card: &str,
    opened: Option<T>,
    there: impl FnOnce(&T) -> bool,
    add: impl FnOnce(&mut Ui, &T),
) {
    let ctx = ui.ctx();
    let id = egui::Id::new(("overview menu", card));
    let opening = opened.is_some();
    if let Some(what) = opened {
        ctx.data_mut(|d| d.insert_temp(id, what));
    }
    if !opening && !egui::Popup::is_id_open(ctx, id) {
        return;
    }
    let Some(what) = ctx.data(|d| d.get_temp::<T>(id)).filter(|w| there(w)) else {
        egui::Popup::close_id(ctx, id);
        return;
    };
    egui::Popup::new(
        id,
        ctx.clone(),
        egui::PopupAnchor::PointerFixed,
        ui.layer_id(),
    )
    .kind(egui::PopupKind::Menu)
    .layout(Layout::top_down_justified(Align::Min))
    .style(egui::containers::menu::menu_style)
    .gap(0.0)
    .open_memory(opening.then_some(egui::SetOpenCommand::Bool(true)))
    // A right click on another row moves it there, open.
    .close_behavior(if opening {
        egui::PopupCloseBehavior::IgnoreClicks
    } else {
        egui::PopupCloseBehavior::CloseOnClick
    })
    .show(|ui| add(ui, &what));
}

/// A name in a row: the accent while the row is pointed at.
fn name(text: &str, hovered: bool) -> RichText {
    RichText::new(text).color(if hovered { theme::ACCENT } else { theme::TEXT })
}

/// "and 12 more", leading to all of them.
fn more(ui: &mut Ui, count: usize, tip: &str) -> bool {
    ui.add(
        Label::new(
            RichText::new(format!("and {count} more"))
                .size(theme::META)
                .color(theme::ACCENT),
        )
        .sense(Sense::click()),
    )
    .on_hover_cursor(CursorIcon::PointingHand)
    .on_hover_text(tip)
    .clicked()
}

fn weak(text: impl Into<String>) -> RichText {
    RichText::new(text)
        .size(theme::META)
        .color(theme::TEXT_WEAK)
}

/// One resource: its total, its history, and who is behind most of it.
struct Tile<'a> {
    title: &'static str,
    /// Beside the title: the processor's clock, a disk's letters.
    aside: String,
    value: String,
    /// The value is out of the ordinary.
    high: bool,
    graph: Graph<'a>,
    view: PerfView,
    /// The app behind most of it and how much; or why none is named.
    culprit: Result<(&'a App, String), &'static str>,
}

fn tiles(ui: &mut Ui, input: &Inputs<'_>, apps: &[App], height: f32, out: &mut Vec<UiCmd>) {
    let snap = input.snap;
    let s = &snap.system;
    let h = &snap.history;
    let m = &s.memory;
    let memory_pct = m.used() as f64 * 100.0 / m.total.max(1) as f64;
    let culprit = |sort: AppSort, show: &dyn Fn(&App) -> String| {
        top(apps, sort).map(|a| (a, show(a))).ok_or("nothing busy")
    };
    let mut tiles = vec![
        Tile {
            title: "CPU",
            aside: if s.cpu.mhz > 0.0 {
                format!("{:.2} GHz", s.cpu.mhz / 1000.0)
            } else {
                String::new()
            },
            value: format!("{:.0} %", s.cpu.total),
            high: s.cpu.total >= 90.0,
            graph: Graph::new(Some(100.0)).series(&h.cpu, theme::CPU, true),
            view: PerfView::Cpu,
            culprit: culprit(AppSort::Cpu, &|a| format!("{:.1} %", a.cpu)),
        },
        Tile {
            title: "Memory",
            aside: format!("{} of {}", fmt::bytes(m.used()), fmt::bytes(m.total)),
            value: format!("{memory_pct:.0} %"),
            high: input.memory_alert.is_some(),
            graph: Graph::new(Some(m.total.max(1) as f32)).series(
                &h.memory_used,
                theme::MEMORY,
                true,
            ),
            view: PerfView::Memory,
            culprit: culprit(AppSort::Memory, &|a| fmt::bytes(a.memory)),
        },
        Tile {
            title: "Disk",
            aside: s
                .disks
                .iter()
                .max_by(|a, b| a.active.total_cmp(&b.active))
                .map_or(String::new(), |d| {
                    format!("busiest {:.0} % active", d.active)
                }),
            value: fmt::rate_always(s.disk_read_rate + s.disk_write_rate),
            high: s.disks.iter().any(|d| d.active >= 90.0),
            graph: Graph::new(None)
                .series(&h.disk_read, theme::DISK_READ, true)
                .series(&h.disk_write, theme::DISK_WRITE, false),
            view: PerfView::Disk,
            culprit: if snap.info_level == InfoLevel::Full {
                culprit(AppSort::Disk, &|a| fmt::rate_always(a.disk))
            } else if cfg!(windows) {
                Err("who: needs administrator rights")
            } else {
                Err("who: needs full access")
            },
        },
        Tile {
            title: "Network",
            aside: format!(
                "R {}  ·  S {}",
                fmt::rate_always(s.net.rx_rate),
                fmt::rate_always(s.net.tx_rate)
            ),
            value: fmt::rate_always(s.net.rx_rate + s.net.tx_rate),
            high: false,
            graph: Graph::new(None)
                .series(&h.net_rx, theme::NET_RX, true)
                .series(&h.net_tx, theme::NET_TX, false),
            view: PerfView::Network,
            culprit: culprit(AppSort::Network, &|a| fmt::rate_always(a.net)),
        },
    ];
    let busiest_gpu = s
        .gpus
        .iter()
        .enumerate()
        .zip(&h.gpus)
        .max_by(|((_, a), _), ((_, b), _)| a.usage.total_cmp(&b.usage));
    if let Some(((i, gpu), gh)) = busiest_gpu {
        tiles.push(Tile {
            title: "GPU",
            aside: short_gpu_name(&gpu.name),
            value: format!("{:.0} %", gpu.usage),
            high: false,
            graph: Graph::new(Some(100.0)).series(&gh.usage, theme::GPU, true),
            view: PerfView::Gpu(i),
            culprit: culprit(AppSort::Gpu, &|a| format!("{:.1} %", a.gpu)),
        });
    }
    let width = ui.available_width();
    let per_row = tiles_per_row(width, tiles.len());
    let tile_width = (width - GAP * (per_row - 1) as f32) / per_row as f32;
    for chunk in tiles.chunks(per_row) {
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = GAP;
            for tile in chunk {
                tile_ui(ui, tile, vec2(tile_width, height), out);
            }
        });
        ui.add_space(GAP - ui.spacing().item_spacing.y);
    }
}

fn tile_ui(ui: &mut Ui, tile: &Tile<'_>, size: egui::Vec2, out: &mut Vec<UiCmd>) {
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let response = response
        .on_hover_cursor(CursorIcon::PointingHand)
        .on_hover_text(format!("{} on the Performance page", tile.title));
    let hovered = response.hovered();
    ui.painter().rect(
        rect,
        theme::RADIUS_PANEL,
        if hovered {
            theme::RAISED
        } else {
            theme::STRIPE
        },
        Stroke::new(
            1.0,
            if hovered {
                theme::BORDER_STRONG
            } else {
                theme::BORDER
            },
        ),
        StrokeKind::Inside,
    );
    widgets::focus_ring(ui, &response);
    if response.clicked() {
        out.push(UiCmd::ShowPerf(tile.view));
    }
    let inner = rect.shrink2(vec2(12.0, 10.0));
    // Inside the tile's rect, taking no more room in the row than it.
    let ui = &mut ui.new_child(
        UiBuilder::new()
            .max_rect(inner)
            .layout(Layout::top_down(Align::Min)),
    );
    {
        ui.spacing_mut().item_spacing.y = 2.0;
        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
        ui.horizontal(|ui| {
            ui.label(widgets::strong(tile.title, theme::META).color(theme::TEXT_WEAK));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add(
                    Label::new(
                        RichText::new(&tile.aside)
                            .size(theme::CAPTION)
                            .color(theme::TEXT_DIM),
                    )
                    .truncate(),
                );
            });
        });
        ui.label(
            widgets::strong(&tile.value, theme::TITLE).color(if tile.high {
                theme::ELEVATED_TEXT
            } else {
                theme::TEXT
            }),
        );
        ui.add_space(2.0);
        let graph_height = (size.y - TILE_TEXT).max(30.0);
        let (graph, _) = ui.allocate_exact_size(vec2(inner.width(), graph_height), Sense::hover());
        tile.graph.paint(ui, graph);
        ui.add_space(4.0);
        ui.horizontal(|ui| match &tile.culprit {
            Ok((app, amount)) => {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(weak(amount.as_str()));
                    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                        if widgets::link(ui, &app.label())
                            .on_hover_text("Show it in the process list")
                            .clicked()
                        {
                            out.push(UiCmd::ShowApp(app.name.clone()));
                        }
                    });
                });
            }
            Err(why) => {
                ui.label(RichText::new(*why).size(theme::META).color(theme::TEXT_DIM));
            }
        });
    }
}

/// "NVIDIA GeForce RTX 5080" -> "RTX 5080".
fn short_gpu_name(name: &str) -> String {
    let name = name.replace("(TM)", "").replace("(R)", "");
    let trimmed = ["NVIDIA GeForce ", "NVIDIA ", "AMD ", "Intel "]
        .iter()
        .find_map(|prefix| name.strip_prefix(prefix))
        .unwrap_or(&name);
    trimmed.trim().to_string()
}

/// Something that needs a look, and what can be done about it.
struct Item {
    /// Red, not amber: it hurts now.
    severe: bool,
    text: String,
    detail: Option<String>,
    /// A button's label and what it asks for.
    actions: Vec<(String, Vec<UiCmd>)>,
}

fn act(label: impl Into<String>, cmd: UiCmd) -> (String, Vec<UiCmd>) {
    (label.into(), vec![cmd])
}

/// Most of each kind listed; the rest are counted.
const EACH: usize = 3;

fn attention(input: &Inputs<'_>, apps: &[App]) -> Vec<Item> {
    let snap = input.snap;
    let s = &snap.system;
    let mut items = Vec::new();
    if let Some((what, pct)) = input.memory_alert {
        let ram = what == "RAM";
        items.push(Item {
            severe: pct >= 95.0,
            text: format!("{what} is {pct:.0} % full"),
            detail: top(apps, AppSort::Memory).map(|a| {
                format!(
                    "The biggest app is {}, with {}.",
                    a.label(),
                    fmt::bytes(a.memory)
                )
            }),
            actions: vec![act("Show apps by memory", UiCmd::AppsByMemory(ram))],
        });
    }
    let live = || {
        snap.processes
            .iter()
            .filter(|p| p.state != ProcState::Exited)
    };
    for p in live().filter(|p| p.not_responding).take(EACH) {
        items.push(Item {
            severe: false,
            text: format!("{} (PID {}) is not responding", p.name, p.pid),
            detail: Some("A window of it has taken no message for 5 seconds.".into()),
            actions: vec![
                act("Why?", UiCmd::ConfirmWaitChain(p.key)),
                act("Show", UiCmd::GoTo(p.key)),
            ],
        });
    }
    for p in live().filter(|p| p.ending).take(EACH) {
        items.push(Item {
            severe: false,
            text: format!("{} (PID {}) is ending but cannot finish", p.name, p.pid),
            detail: Some(if cfg!(windows) {
                "Something still holds it; its details say what.".into()
            } else {
                "Its parent has not collected it; its details say which.".into()
            }),
            actions: vec![act("Show", UiCmd::GoTo(p.key))],
        });
    }
    for runaway in input.runaways {
        items.push(Item {
            severe: false,
            text: runaway.headline(),
            detail: Some(format!(
                "Together they use {} of memory and {:.1} % CPU.",
                fmt::bytes(runaway.memory),
                runaway.cpu
            )),
            actions: runaway_actions(runaway),
        });
    }
    for d in s.disks.iter().filter(|d| d.active >= 90.0) {
        items.push(Item {
            severe: false,
            text: format!("Disk {} is {:.0} % active", d.number, d.active),
            detail: Some(format!(
                "{}: reading {}, writing {}, {:.1} ms a request.",
                d.model,
                fmt::rate_always(d.read_rate),
                fmt::rate_always(d.write_rate),
                d.response_ms
            )),
            actions: vec![act(
                "Who?",
                UiCmd::ShowPerf(if s.disks.len() > 1 {
                    PerfView::PhysicalDisk(d.number)
                } else {
                    PerfView::Disk
                }),
            )],
        });
    }
    for (d, v) in s
        .disks
        .iter()
        .flat_map(|d| d.volumes.iter().map(move |v| (d, v)))
        .filter(|(_, v)| v.total > 0 && v.free < LOW_SPACE && v.free < v.total / 10)
    {
        let place = if v.letter.ends_with(':') {
            format!("{}\\", v.letter)
        } else {
            v.letter.clone()
        };
        items.push(Item {
            severe: v.free < v.total / 50,
            text: format!("{} has {} free", v.letter, fmt::bytes(v.free)),
            detail: Some(format!("Of {} on disk {}.", fmt::bytes(v.total), d.number)),
            actions: vec![act("Open", UiCmd::OpenLocation(place))],
        });
    }
    for t in s
        .sensors
        .iter()
        .filter(|t| t.kind == SensorKind::Temperature && t.heat() == Heat::Hot)
    {
        items.push(Item {
            severe: t.critical.is_some_and(|c| t.value >= c - 3.0),
            text: format!("{} {} is at {:.0} °C", t.device, t.label, t.value),
            detail: t
                .critical
                .map(|c| format!("Its critical temperature is {c:.0} °C.")),
            actions: vec![act("Show", UiCmd::ShowPerf(PerfView::Sensors))],
        });
    }
    let mut leaked: Vec<ProcKey> = Vec::new();
    for l in input.leaks {
        if leaked.contains(&l.key) {
            continue;
        }
        leaked.push(l.key);
        if leaked.len() > EACH {
            break;
        }
        items.push(Item {
            severe: false,
            text: format!("{} (PID {}) keeps growing", l.name, l.key.pid),
            detail: Some(fmt::leak(l)),
            actions: vec![act("Show", UiCmd::GoTo(l.key))],
        });
    }
    // The worst first; within a kind, as found.
    items.sort_by_key(|i| !i.severe);
    items
}

fn runaway_actions(runaway: &Runaway) -> Vec<(String, Vec<UiCmd>)> {
    let show = UiCmd::ShowCopies {
        name: runaway.name.clone(),
        starter: runaway.starter.as_ref().map(|s| s.0),
    };
    vec![
        act("Show", show),
        act(
            format!("End all {}\u{2026}", runaway.copies.len()),
            UiCmd::KillMany {
                title: format!(
                    "Terminate {} copies of {}?",
                    runaway.copies.len(),
                    runaway.name
                ),
                subject: runaway.headline(),
                keys: runaway.copies.clone(),
            },
        ),
        act(
            format!("Don't warn about {}", runaway.name),
            UiCmd::IgnoreRunaway(runaway.id.clone()),
        ),
    ]
}

fn main_column(
    ui: &mut Ui,
    page: &mut OverviewPage,
    input: &Inputs<'_>,
    apps: &[App],
    icons: &mut IconCache,
    out: &mut Vec<UiCmd>,
) {
    let items = attention(input, apps);
    let title = if items.is_empty() {
        "Needs attention".to_string()
    } else {
        format!("Needs attention  ·  {}", items.len())
    };
    card(
        ui,
        &title,
        |_| {},
        |ui| {
            if items.is_empty() {
                ui.horizontal(|ui| {
                    ui.label(widgets::icon(theme::icon::CHECK, 13.0).color(theme::GOOD));
                    ui.label("Nothing needs attention");
                });
                ui.label(weak(
                    "Memory, disks, temperatures, programs that hang or will not end, ones that \
                     keep growing or run in many copies: all as they should be.",
                ));
                return;
            }
            for (i, item) in items.into_iter().enumerate() {
                if i > 0 {
                    ui.add_space(4.0);
                    ui.separator();
                    ui.add_space(2.0);
                }
                attention_row(ui, item, out);
            }
        },
    );
    ui.add_space(GAP);
    busiest(ui, page, input, apps, icons, out);
    ui.add_space(GAP);
    recent(ui, input, out);
}

fn attention_row(ui: &mut Ui, item: Item, out: &mut Vec<UiCmd>) {
    let color = if item.severe {
        theme::BAD
    } else {
        theme::ELEVATED_TEXT
    };
    ui.horizontal_top(|ui| {
        ui.label(widgets::icon(theme::icon::WARNING, 13.0).color(color));
        ui.vertical(|ui| {
            ui.add(Label::new(RichText::new(&item.text).color(theme::TEXT)).wrap());
            if let Some(detail) = &item.detail {
                ui.add(Label::new(weak(detail.as_str())).wrap());
            }
            ui.horizontal_wrapped(|ui| {
                for (label, cmds) in item.actions {
                    if ui.small_button(label).clicked() {
                        out.extend(cmds);
                    }
                }
            });
        });
    });
}

fn busiest(
    ui: &mut Ui,
    page: &mut OverviewPage,
    input: &Inputs<'_>,
    apps: &[App],
    icons: &mut IconCache,
    out: &mut Vec<UiCmd>,
) {
    let mut sorted: Vec<&App> = apps.iter().collect();
    let sort = page.sort;
    sorted.sort_by(|a, b| {
        b.value(sort)
            .total_cmp(&a.value(sort))
            .then(b.memory.cmp(&a.memory))
    });
    let disk_known = input.snap.info_level == InfoLevel::Full;
    card(
        ui,
        "Busiest apps",
        |ui| {
            ui.label(weak("Every process of a program together"));
        },
        |ui| {
            let numbers: [(&str, f32, Option<AppSort>); 5] = [
                ("CPU", 64.0, Some(AppSort::Cpu)),
                ("Memory", 84.0, Some(AppSort::Memory)),
                ("Disk", 84.0, Some(AppSort::Disk)),
                ("Network", 84.0, Some(AppSort::Network)),
                ("GPU", 56.0, Some(AppSort::Gpu)),
            ];
            let fixed: f32 = numbers.iter().map(|n| n.1).sum::<f32>()
                + ui.spacing().item_spacing.x * numbers.len() as f32;
            let name_width = (ui.available_width() - fixed).max(140.0);
            ui.horizontal(|ui| {
                widgets::text(
                    ui,
                    name_width,
                    Align::Min,
                    widgets::strong("App", theme::META).color(theme::TEXT_WEAK),
                );
                for (title, width, by) in numbers {
                    let chosen = by == Some(sort);
                    widgets::cell(ui, width, Align::Max, |ui| {
                        let text = widgets::strong(title, theme::META).color(if chosen {
                            theme::TEXT
                        } else {
                            theme::TEXT_WEAK
                        });
                        let response = ui
                            .add(Label::new(text).sense(Sense::click()))
                            .on_hover_cursor(CursorIcon::PointingHand)
                            .on_hover_text(format!("Busiest by {} first", title.to_lowercase()));
                        if chosen {
                            ui.label(
                                widgets::icon(theme::icon::CHEVRON_DOWN, 9.0).color(theme::ACCENT),
                            );
                        }
                        if response.clicked()
                            && let Some(by) = by
                        {
                            page.sort = by;
                        }
                    });
                }
            });
            let rule = ui.min_rect();
            ui.painter().hline(
                rule.x_range(),
                ui.cursor().top(),
                Stroke::new(1.0, theme::BORDER),
            );
            ui.add_space(3.0);
            let shown = sorted.len().min(APP_ROWS);
            let mut opened = None;
            for (i, app) in sorted.into_iter().take(APP_ROWS).enumerate() {
                let response = row(ui, &app.name, i, |ui, hovered| {
                    widgets::cell(ui, name_width, Align::Min, |ui| {
                        app_icon(ui, input.details, icons, app.key);
                        ui.add(Label::new(name(&app.label(), hovered)).truncate());
                    });
                    let number = |ui: &mut Ui, width: f32, text: String| {
                        widgets::text(ui, width, Align::Max, text);
                    };
                    number(ui, numbers[0].1, fmt::cpu(app.cpu));
                    number(ui, numbers[1].1, fmt::bytes(app.memory));
                    number(
                        ui,
                        numbers[2].1,
                        if disk_known {
                            fmt::rate(app.disk)
                        } else {
                            String::new()
                        },
                    );
                    number(ui, numbers[3].1, fmt::rate(app.net));
                    number(ui, numbers[4].1, fmt::cpu(app.gpu));
                });
                let response = response.on_hover_text(if app.count > 1 {
                    "Its processes in the list; right click for more"
                } else {
                    "It in the list; right click for more"
                });
                if response.clicked() {
                    out.push(UiCmd::ShowApp(app.name.clone()));
                }
                if response.secondary_clicked() {
                    opened = Some(app.name.clone());
                }
            }
            for _ in shown..APP_ROWS {
                ui.add_space(ROW_HEIGHT + ui.spacing().item_spacing.y);
            }
            let find = |name: &Arc<str>| apps.iter().find(|a| a.name.eq_ignore_ascii_case(name));
            held_menu(
                ui,
                "app",
                opened,
                |name| find(name).is_some(),
                |ui, name| {
                    if let Some(app) = find(name) {
                        app_menu(ui, app, input, out);
                    }
                },
            );
        },
    );
}

/// What can be done with every process of a program.
fn app_menu(ui: &mut Ui, app: &App, input: &Inputs<'_>, out: &mut Vec<UiCmd>) {
    use crate::proc_table::item;
    ui.set_min_width(220.0);
    let processes = if app.count == 1 {
        "1 process".to_string()
    } else {
        format!("{} processes", app.count)
    };
    ui.label(RichText::new(format!("{}  ·  {processes}", app.name)).color(theme::TEXT_WEAK));
    ui.separator();
    item(
        ui,
        out,
        "Show in the process list",
        "",
        UiCmd::ShowApp(app.name.clone()),
    );
    let (label, title, subject) = if app.count == 1 {
        (
            "Terminate\u{2026}".to_string(),
            "Terminate process?".to_string(),
            format!("{} (PID {})", app.name, app.key.pid),
        )
    } else {
        (
            format!("Terminate all {}\u{2026}", app.count),
            format!("Terminate all {} processes?", app.count),
            app.name.to_string(),
        )
    };
    item(
        ui,
        out,
        &label,
        "",
        UiCmd::KillMany {
            title,
            subject,
            keys: app.keys.clone(),
        },
    );
    if app.count == 1 {
        let watched = input.exit_watched.contains(&app.key);
        item(
            ui,
            out,
            if watched {
                "Don't notify me when it exits"
            } else {
                "Notify me when it exits"
            },
            "",
            UiCmd::NotifyExit(app.key, !watched),
        );
    }
    item(
        ui,
        out,
        &format!("Create a rule for {}\u{2026}", app.name),
        "",
        UiCmd::NewRule {
            image: app.name.clone(),
            base_priority: app.base_priority,
        },
    );
    ui.separator();
    item(
        ui,
        out,
        "Search online",
        "",
        UiCmd::SearchOnline(app.name.to_string()),
    );
}

fn app_icon(ui: &mut Ui, details: &Details, icons: &mut IconCache, key: ProcKey) {
    match details.get(&key).and_then(|d| d.icon.as_ref()) {
        Some(icon) => {
            let id = icons.texture(ui.ctx(), icon);
            ui.image((id, vec2(16.0, 16.0)));
        }
        None => {
            let (rect, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
            ui.painter().rect_stroke(
                rect.shrink(2.5),
                2.0,
                Stroke::new(1.0, theme::TEXT_DIM),
                StrokeKind::Inside,
            );
        }
    }
}

fn side_column(ui: &mut Ui, input: &Inputs<'_>, out: &mut Vec<UiCmd>) {
    card(
        ui,
        "Quick actions",
        |_| {},
        |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
                let button = |ui: &mut Ui, glyph: &str, label: &str, tip: &str| {
                    ui.add(egui::Button::new(widgets::icon_text(ui, glyph, label)))
                        .on_hover_text(tip)
                        .clicked()
                };
                if button(
                    ui,
                    theme::icon::RUN,
                    "Run new task\u{2026}",
                    "Start a program, with or without administrator rights (Ctrl+N)",
                ) {
                    out.push(UiCmd::RunTask);
                }
                if button(
                    ui,
                    theme::icon::SEARCH,
                    "Find handle\u{2026}",
                    "Who has this file, key or object open? (Ctrl+H)",
                ) {
                    out.push(UiCmd::OpenFind);
                }
                if button(
                    ui,
                    theme::icon::NETWORK,
                    "Free a port\u{2026}",
                    "The ports programs listen on, and who holds the one you need",
                ) {
                    out.push(UiCmd::ShowListening);
                }
                if button(
                    ui,
                    theme::icon::RECORD,
                    "Record a program\u{2026}",
                    "What a program does, step by step: files, registry, network, processes",
                ) {
                    out.push(UiCmd::ShowTab(Tab::Activity));
                }
                if !input.elevated
                    && ui
                        .add_enabled(
                            !input.relaunching,
                            egui::Button::new(widgets::icon_text(
                                ui,
                                theme::icon::SHIELD,
                                crate::platform::ELEVATE,
                            )),
                        )
                        .on_hover_text(crate::platform::PLAIN_TIP)
                        .on_disabled_hover_text(crate::platform::ELEVATING)
                        .clicked()
                {
                    out.push(UiCmd::RelaunchElevated);
                }
            });
        },
    );
    if let Some(activity) = &input.snap.system.net_activity {
        ui.add_space(GAP);
        development(ui, input, activity, out);
    }
    ui.add_space(GAP);
    this_pc(ui, input);
}

fn development(ui: &mut Ui, input: &Inputs<'_>, activity: &NetActivity, out: &mut Vec<UiCmd>) {
    let watched = input.watched_ports;
    let mut ports = crate::network_page::dev_ports(input.snap, activity, watched);
    // Yours first.
    ports.sort_by_key(|p| (!p.watched, p.port, p.pid));
    let (mut all, mut rest) = (false, false);
    card(
        ui,
        "Development ports",
        |ui| {
            all = ui
                .small_button("Show all")
                .on_hover_text("Network > Development ports: every one, with the ones you watch")
                .clicked();
        },
        |ui| {
            if ports.is_empty() {
                ui.label(weak(
                    "No dev server, database or debugger is listening. Ports you watch \
                     (Network > Development ports, or Settings) show here too.",
                ));
                return;
            }
            let mut opened = None;
            for (i, p) in ports.iter().take(PORT_ROWS).enumerate() {
                let response = row(ui, (p.port, p.pid), i, |ui, hovered| {
                    widgets::text(
                        ui,
                        54.0,
                        Align::Max,
                        widgets::strong(format!(":{}", p.port), theme::BODY),
                    );
                    widgets::text(
                        ui,
                        120.0,
                        Align::Min,
                        RichText::new(&p.tool).color(theme::TEXT_WEAK),
                    );
                    if p.pid == 0 {
                        ui.label(RichText::new("free").color(theme::GOOD));
                        return;
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui
                            .small_button("Free\u{2026}")
                            .on_hover_text(format!("End {} so the port is free", p.program))
                            .clicked()
                        {
                            out.push(UiCmd::FreePort {
                                port: p.port,
                                pid: p.pid,
                            });
                        }
                        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                            ui.add(
                                Label::new(name(&format!("{} ({})", p.program, p.pid), hovered))
                                    .truncate(),
                            );
                        });
                    });
                });
                if let Some(key) = holder(input.snap, p.pid)
                    && response.clicked()
                {
                    out.push(UiCmd::GoTo(key));
                }
                if response.secondary_clicked() {
                    opened = Some((p.port, p.pid));
                }
            }
            rest = ports.len() > PORT_ROWS && more(ui, ports.len() - PORT_ROWS, "All of them");
            let find = |what: &(u16, u32)| ports.iter().find(|p| (p.port, p.pid) == *what);
            held_menu(
                ui,
                "port",
                opened,
                |what| find(what).is_some(),
                |ui, what| {
                    if let Some(p) = find(what) {
                        port_menu(ui, p, holder(input.snap, p.pid), out);
                    }
                },
            );
        },
    );
    if all || rest {
        out.push(UiCmd::ShowDevPorts);
    }
}

/// The live process with this PID: what holds a port.
fn holder(snap: &Snapshot, pid: u32) -> Option<ProcKey> {
    snap.processes
        .iter()
        .find(|q| q.pid == pid && pid != 0 && q.state != ProcState::Exited)
        .map(|q| q.key)
}

/// What can be done about a port.
fn port_menu(
    ui: &mut Ui,
    p: &crate::network_page::DevPort,
    key: Option<ProcKey>,
    out: &mut Vec<UiCmd>,
) {
    use crate::proc_table::item;
    ui.set_min_width(200.0);
    ui.label(RichText::new(format!(":{}  ·  {}", p.port, p.tool)).color(theme::TEXT_WEAK));
    ui.separator();
    if let Some(key) = key {
        item(
            ui,
            out,
            &format!("Go to {}", p.program),
            "",
            UiCmd::GoTo(key),
        );
        item(
            ui,
            out,
            &format!("Free port {}\u{2026}", p.port),
            "",
            UiCmd::FreePort {
                port: p.port,
                pid: p.pid,
            },
        );
        ui.separator();
    }
    if p.watched {
        item(
            ui,
            out,
            "Stop watching this port",
            "",
            UiCmd::UnwatchPort(p.port),
        );
    } else {
        item(
            ui,
            out,
            "Watch this port",
            "",
            UiCmd::WatchPort(crate::settings::PortWatch {
                port: p.port,
                name: String::new(),
            }),
        );
    }
    item(ui, out, "Copy port", "", UiCmd::Copy(p.port.to_string()));
}

fn recent(ui: &mut Ui, input: &Inputs<'_>, out: &mut Vec<UiCmd>) {
    let snap = input.snap;
    let now = anabolic_core::sysinfo::now_filetime();
    let since = now - RECENT_MINUTES * 60 * 10_000_000;
    // A console's host comes with every console program: noise here.
    let mut started: Vec<_> = snap
        .processes
        .iter()
        .filter(|p| p.state != ProcState::Exited && !p.is_pseudo() && p.key.create_time > since)
        .filter(|p| !p.name.eq_ignore_ascii_case("conhost.exe"))
        .collect();
    started.sort_by_key(|p| std::cmp::Reverse(p.key.create_time));
    card(
        ui,
        "Recently started",
        |ui| {
            ui.label(weak(format!("the last {RECENT_MINUTES} minutes")));
        },
        |ui| {
            if started.is_empty() {
                ui.label(weak("Nothing has started since."));
                return;
            }
            let shown = started.len().min(RECENT_ROWS);
            let mut opened = None;
            for (i, p) in started.iter().take(RECENT_ROWS).enumerate() {
                let ago =
                    Duration::from_secs(((now - p.key.create_time).max(0) / 10_000_000) as u64);
                let parent = snap
                    .processes
                    .iter()
                    .find(|q| q.pid == p.ppid && q.key.create_time <= p.key.create_time);
                let response = row(ui, p.key, i, |ui, hovered| {
                    let when = if ago.as_secs() < 2 {
                        "now".to_string()
                    } else {
                        fmt::span(ago)
                    };
                    widgets::text(ui, 52.0, Align::Max, weak(when));
                    ui.add_space(4.0);
                    ui.add(Label::new(name(&p.name, hovered)));
                    if let Some(parent) = parent {
                        ui.add(Label::new(weak(format!("from {}", parent.name))).truncate());
                    }
                });
                let response = match input.cmdlines.get(&p.key) {
                    Some(cmd) => response.on_hover_text(&**cmd),
                    None => response,
                };
                if response.clicked() {
                    out.push(UiCmd::GoTo(p.key));
                }
                if response.secondary_clicked() {
                    opened = Some(p.key);
                }
            }
            // Once older than the card goes back, or below its last row,
            // the process' menu stays open: it is still running.
            let find = |key: &ProcKey| {
                snap.processes
                    .iter()
                    .find(|q| q.key == *key && q.state != ProcState::Exited)
            };
            held_menu(
                ui,
                "process",
                opened,
                |key| find(key).is_some(),
                |ui, key| {
                    if let Some(p) = find(key) {
                        let menu = crate::proc_table::Menu {
                            blocked: input.blocked,
                            secrets: input.secrets,
                            exit_watched: input.exit_watched.contains(&p.key),
                        };
                        let d = input.details.get(&p.key);
                        crate::proc_table::context_menu(ui, p, d, &menu, out);
                    }
                },
            );
            if started.len() > RECENT_ROWS {
                if more(
                    ui,
                    started.len() - RECENT_ROWS,
                    "All of them in the process list",
                ) {
                    out.push(UiCmd::ShowNew);
                }
            } else {
                for _ in shown..RECENT_ROWS {
                    ui.add_space(ROW_HEIGHT + ui.spacing().item_spacing.y);
                }
            }
        },
    );
}

fn this_pc(ui: &mut Ui, input: &Inputs<'_>) {
    let s = &input.snap.system;
    let hw = input.hw;
    card(
        ui,
        "This PC",
        |_| {},
        |ui| {
            let fact = |ui: &mut Ui, label: &str, value: String| {
                ui.horizontal(|ui| {
                    widgets::text(ui, 86.0, Align::Min, weak(label));
                    ui.add(Label::new(value).truncate());
                });
            };
            if !input.cpu_name.is_empty() {
                fact(ui, "Processor", input.cpu_name.to_string());
            }
            if hw.cpu.cores > 0 {
                fact(
                    ui,
                    "Cores",
                    format!("{} cores  ·  {} threads", hw.cpu.cores, hw.cpu.threads),
                );
            }
            let modules = &hw.memory.modules;
            fact(
                ui,
                "Memory",
                match modules.first() {
                    Some(m) => format!(
                        "{}  ·  {}  ·  {} MT/s",
                        fmt::bytes(s.memory.total),
                        m.kind,
                        if m.configured_speed > 0 {
                            m.configured_speed
                        } else {
                            m.speed
                        }
                    ),
                    None => fmt::bytes(s.memory.total),
                },
            );
            if !s.gpus.is_empty() {
                let names: Vec<String> = s.gpus.iter().map(|g| short_gpu_name(&g.name)).collect();
                fact(ui, "Graphics", names.join("  ·  "));
            }
            fact(
                ui,
                "Running",
                format!(
                    "{} processes  ·  {} threads  ·  {} handles",
                    fmt::count(s.processes as u64),
                    fmt::count(s.threads as u64),
                    fmt::count(s.handles as u64)
                ),
            );
            fact(
                ui,
                "Anabolic",
                if input.elevated {
                    crate::platform::RUNNING_ELEVATED.to_string()
                } else {
                    crate::platform::RUNNING_PLAIN.to_string()
                },
            );
        },
    );
}

/// All in one row while each gets 170 px; on a narrow window three, then two.
fn tiles_per_row(width: f32, n: usize) -> usize {
    match n {
        n if width >= 170.0 * n as f32 + GAP * (n - 1) as f32 => n,
        _ if width >= 520.0 => 3,
        _ => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiles_wrap_on_a_narrow_window() {
        assert_eq!(tiles_per_row(1400.0, 5), 5);
        assert_eq!(tiles_per_row(800.0, 5), 3);
        assert_eq!(tiles_per_row(400.0, 5), 2);
    }

    #[test]
    fn shortens_gpu_names() {
        assert_eq!(short_gpu_name("NVIDIA GeForce RTX 5080"), "RTX 5080");
        assert_eq!(short_gpu_name("AMD Radeon(TM) Graphics"), "Radeon Graphics");
        assert_eq!(
            uptime(Duration::from_secs(3 * 86_400 + 4 * 3600 + 5)),
            "3 d 4 h"
        );
        assert_eq!(uptime(Duration::from_secs(300)), "5 min");
    }
}

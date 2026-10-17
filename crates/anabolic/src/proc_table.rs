//! The process list: a virtualized table (only visible rows are laid out),
//! tree indentation or app groups, heat shading and the context menus.

use std::collections::HashSet;
use std::sync::Arc;

use anabolic_core::actions::Priority;
use anabolic_core::{InfoLevel, ProcKey, ProcState, ProcessDetails, ProcessInfo, Snapshot};
use eframe::egui::{self, Align, Color32, Label, Layout, RichText, Sense, Shape, Stroke, Ui, vec2};
use egui_extras::{Column, TableBuilder};

use crate::fmt;
use crate::icons::IconCache;
use crate::settings::{Col, Settings};
use crate::theme;
use crate::tree::{self, BuildInput, Details, Group, Row};
use crate::widgets;

pub const ROW_HEIGHT: f32 = 20.0;
const INDENT: f32 = 14.0;
/// Refreshes a new or exited process stays highlighted for.
const HIGHLIGHT_TICKS: u64 = 2;

/// Everything the UI asks the app to do; views never act themselves.
pub enum UiCmd {
    Select(Option<ProcKey>),
    SelectGroup(Arc<str>),
    /// Switch to the process list and select (used from other tabs).
    GoTo(ProcKey),
    /// The same, by PID (a service's host process).
    GoToPid(u32),
    /// Start, stop or reconfigure a service.
    Service {
        name: Arc<str>,
        display: Arc<str>,
        op: anabolic_core::services::ServiceOp,
    },
    /// Select a service on the Services page.
    SelectService(Arc<str>),
    /// Turn a startup entry on or off.
    SetStartup(Box<anabolic_core::startup::StartupItem>, bool),
    /// Start Anabolic again as administrator.
    RelaunchElevated,
    /// Open the Network page.
    ShowNetwork,
    /// What a process does, live, on the Activity page (with the processes
    /// under it in the tree view).
    LiveActivity(ProcKey),
    /// The same for every process of an Apps group.
    LiveActivityGroup(Arc<str>),
    /// Ask before cutting a program off the network.
    ConfirmBlock {
        path: String,
        name: Arc<str>,
    },
    /// Give a blocked program its network back.
    Unblock(String),
    /// Drop one TCP connection of a process (IPv4).
    CloseConnection {
        pid: u32,
        process: Arc<str>,
        remote: std::net::Ipv4Addr,
        remote_port: u16,
        local_port: u16,
    },
    /// Open the Services page on the services of one process.
    ShowServices(u32),
    /// Like `GoTo`, then show this thread in the Threads tab.
    GoToThread {
        key: ProcKey,
        tid: u32,
    },
    ToggleCollapse(ProcKey),
    ToggleGroup(Arc<str>),
    Kill {
        key: ProcKey,
        tree: bool,
    },
    KillGroup(Arc<str>),
    Suspend(ProcKey),
    Resume(ProcKey),
    SetPriority(ProcKey, Priority),
    Efficiency(ProcKey, bool),
    AffinityDialog(ProcKey),
    OpenLocation(String),
    Properties(String),
    SearchOnline(String),
    Copy(String),
    Sort(Col),
    ToggleColumn(Col),
    ResetColumns,
    InspectHandles {
        key: ProcKey,
        names: bool,
    },
    /// Ask before duplicating every handle of the process.
    ConfirmResolveNames(ProcKey),
    /// Ask before reading the process' module list (opens it for reading).
    ConfirmLoadModules(ProcKey),
    /// Ask before writing a memory dump of the process.
    ConfirmDump(ProcKey, anabolic_core::dump::DumpKind),
    /// Ask before following what the process' threads wait for.
    ConfirmWaitChain(ProcKey),
    /// Drop the wait chain analysis under way.
    StopWaitChain,
    /// Find out why the process does not go away.
    WhyAlive(ProcKey),
    /// Suspend, resume or end one thread, or set its priority.
    Thread {
        key: ProcKey,
        tid: u32,
        op: anabolic_core::threads::ThreadOp,
    },
    /// Ask before ending one thread.
    ConfirmEndThread {
        key: ProcKey,
        tid: u32,
    },
    /// Read a thread's call stack (asks once per process).
    ConfirmStack {
        key: ProcKey,
        tid: u32,
    },
    /// Ask before following what one thread waits for.
    ConfirmThreadWaitChain {
        key: ProcKey,
        tid: u32,
    },
    /// Signature, hash and build facts of a file.
    FileFacts(Arc<str>),
    /// Who else holds a kernel object.
    Holders {
        key: ProcKey,
        object: u64,
    },
    OpenUrl(String),
    /// Open the find window and search for this text.
    FindText(String),
    /// Keep running in the notification area when the window closes.
    CloseToTray(bool),
    /// Ask before closing handles inside another process.
    ConfirmCloseHandles {
        pid: u32,
        process: Arc<str>,
        handles: Vec<u64>,
        what: String,
    },
    /// Open the rule editor for this program, at its current priority.
    NewRule {
        image: Arc<str>,
        base_priority: i32,
    },
    /// Load modules without asking (the System process: no process is opened).
    LoadModules(ProcKey),
    OpenFind,
    FindHandles {
        query: String,
        exclude: Vec<String>,
    },
    ConfirmCloseHandle(anabolic_core::handles::FoundHandle),
    FindZombies,
}

#[derive(Default)]
pub struct ProcTable {
    pub rows: Vec<Row>,
    pub groups: Vec<Group>,
    pub collapsed: HashSet<ProcKey>,
    pub expanded_groups: HashSet<Arc<str>>,
    pub selected: Option<ProcKey>,
    pub selected_group: Option<Arc<str>>,
    pub filter: String,
    pub dirty: bool,
    pub scroll_to_selected: bool,
    /// Shift+F10: open the selected row's menu once the row is drawn.
    pub open_menu: std::cell::Cell<bool>,
}

impl ProcTable {
    pub fn rebuild(&mut self, snap: &Snapshot, details: &Details, settings: &Settings) {
        let filter = self.filter.trim().to_lowercase();
        let built = tree::build(&BuildInput {
            procs: &snap.processes,
            details,
            sort: settings.sort,
            descending: settings.descending,
            view: settings.view,
            collapsed: &self.collapsed,
            expanded_groups: &self.expanded_groups,
            filter: &filter,
        });
        self.rows = built.rows;
        self.groups = built.groups;
        self.dirty = false;
    }

    pub fn group(&self, id: &str) -> Option<&Group> {
        self.groups.iter().find(|g| &*g.id == id)
    }

    pub fn selected_row(&self, snap: &Snapshot) -> Option<usize> {
        self.rows.iter().position(|r| match r.group {
            Some(g) => self.selected_group.as_deref() == Some(&*self.groups[g as usize].id),
            None => {
                self.selected_group.is_none()
                    && self.selected == Some(snap.processes[r.idx as usize].key)
            }
        })
    }
}

struct Ctx<'a> {
    snap: &'a Snapshot,
    cores: f32,
    total_memory: f64,
}

pub fn show(
    ui: &mut Ui,
    table: &mut ProcTable,
    snap: &Snapshot,
    details: &Details,
    icons: &mut IconCache,
    settings: &Settings,
    out: &mut Vec<UiCmd>,
) {
    if table.rows.is_empty() {
        ui.add_space(40.0);
        ui.vertical_centered(|ui| {
            let msg = if table.filter.trim().is_empty() {
                "Waiting for the first sample…".to_string()
            } else {
                format!("No process matches “{}”", table.filter.trim())
            };
            ui.label(RichText::new(msg).color(theme::TEXT_WEAK));
        });
        return;
    }

    let ctx = Ctx {
        snap,
        cores: snap.system.cores.len().max(1) as f32,
        total_memory: snap.system.memory.total.max(1) as f64,
    };
    let cols = &settings.columns;
    let mut builder = TableBuilder::new(ui)
        .id_salt("processes")
        .striped(true)
        .resizable(true)
        .sense(Sense::click())
        .cell_layout(Layout::left_to_right(Align::Center))
        .auto_shrink([false, false])
        .max_scroll_height(f32::INFINITY);
    for (i, c) in cols.iter().enumerate() {
        builder = builder.column(if i + 1 == cols.len() {
            Column::remainder().at_least(60.0).clip(true)
        } else {
            Column::initial(c.initial_width()).at_least(36.0).clip(true)
        });
    }
    if std::mem::take(&mut table.scroll_to_selected)
        && let Some(row) = table.selected_row(snap)
    {
        builder = builder.scroll_to_row(row, None);
    }
    let table = &*table;

    builder
        .header(ROW_HEIGHT + 2.0, |mut header| {
            for &c in cols {
                let (_, response) = header.col(|ui| header_cell(ui, c, settings));
                if response.clicked() {
                    out.push(UiCmd::Sort(c));
                }
                response.context_menu(|ui| column_chooser(ui, settings, out));
                if !c.tooltip().is_empty() {
                    response.on_hover_text(c.tooltip());
                }
            }
        })
        .body(|body| {
            body.rows(ROW_HEIGHT, table.rows.len(), |mut row| {
                let r = table.rows[row.index()];
                let group = r.group.map(|g| &table.groups[g as usize]);
                let first = &snap.processes[r.idx as usize];
                let p = group.map_or(first, |g| &g.agg);
                let d = details.get(&first.key);
                let selected = match group {
                    Some(g) => table.selected_group.as_deref() == Some(&*g.id),
                    None => table.selected_group.is_none() && table.selected == Some(p.key),
                };
                row.set_selected(selected);
                let tint = if group.is_some() {
                    None
                } else {
                    row_tint(p, snap.seq)
                };
                for &c in cols {
                    let (_, cell) = row.col(|ui| {
                        if let Some(tint) = tint {
                            ui.painter().rect_filled(
                                ui.max_rect().expand2(vec2(4.0, 1.0)),
                                0.0,
                                tint,
                            );
                        }
                        if c == Col::Name {
                            name_cell(ui, p, d, r, group, icons, out);
                        } else {
                            value_cell(ui, c, p, d, group.is_some(), selected, &ctx);
                        }
                    });
                    if c == Col::Name
                        && group.is_none()
                        && let Some(d) = d
                    {
                        cell.on_hover_ui(|ui| name_tooltip(ui, p, d));
                    }
                }
                let response = row.response();
                match group {
                    Some(g) => {
                        if response.clicked() || response.secondary_clicked() {
                            out.push(UiCmd::SelectGroup(g.id.clone()));
                        }
                        if response.double_clicked() {
                            out.push(UiCmd::ToggleGroup(g.id.clone()));
                        }
                        response.context_menu(|ui| group_menu(ui, g, d, out));
                        let open = selected && table.open_menu.replace(false);
                        keyboard_menu(&response, open, |ui| group_menu(ui, g, d, out));
                    }
                    None => {
                        if response.clicked() || response.secondary_clicked() {
                            out.push(UiCmd::Select(Some(p.key)));
                        }
                        if response.double_clicked() && r.has_children {
                            out.push(UiCmd::ToggleCollapse(p.key));
                        }
                        let blocked = &settings.blocked;
                        response.context_menu(|ui| context_menu(ui, p, d, blocked, out));
                        let open = selected && table.open_menu.replace(false);
                        keyboard_menu(&response, open, |ui| context_menu(ui, p, d, blocked, out));
                    }
                }
            });
        });
}

/// The row's menu opened from the keyboard (Shift+F10): under the row, with
/// its first item focused so the arrows and Enter work from there.
fn keyboard_menu(response: &egui::Response, open: bool, add_contents: impl FnOnce(&mut Ui)) {
    let id = response.id.with("keyboard menu");
    if open {
        egui::Popup::open_id(&response.ctx, id);
    }
    egui::Popup::menu(response)
        .id(id)
        .open_memory(None)
        .anchor(egui::PopupAnchor::Position(
            response.rect.left_bottom() + vec2(24.0, 2.0),
        ))
        .show(|ui| {
            if open {
                ui.memory_mut(|m| {
                    if let Some(focused) = m.focused() {
                        m.surrender_focus(focused);
                    }
                    m.move_focus(egui::FocusDirection::Next);
                });
            }
            add_contents(ui);
        });
}

fn header_cell(ui: &mut Ui, c: Col, settings: &Settings) {
    let sorted = settings.sort == c;
    let text = widgets::strong(c.title(), theme::META).color(if sorted {
        theme::TEXT
    } else {
        theme::TEXT_WEAK
    });
    // The arrow sits beside the title, never inside it, so the title does
    // not move when the sort column changes.
    let arrow = sorted.then(|| {
        let glyph = if settings.descending {
            theme::icon::CHEVRON_DOWN
        } else {
            theme::icon::CHEVRON_UP
        };
        widgets::icon(glyph, 9.0).color(theme::ACCENT)
    });
    if c.numeric() {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.add(Label::new(text).truncate());
            if let Some(arrow) = arrow {
                ui.label(arrow);
            }
        });
    } else {
        if c != Col::Name {
            ui.add_space(TEXT_INSET);
        }
        ui.add(Label::new(text).truncate());
        if let Some(arrow) = arrow {
            ui.label(arrow);
        }
    }
}

/// Left inset of text columns, so they do not touch a right-aligned number.
const TEXT_INSET: f32 = 8.0;

fn column_chooser(ui: &mut Ui, settings: &Settings, out: &mut Vec<UiCmd>) {
    ui.label(widgets::strong("Columns", theme::BODY));
    for c in Col::ALL {
        if c == Col::Name {
            continue;
        }
        let mut on = settings.columns.contains(&c);
        if ui.checkbox(&mut on, c.title()).clicked() {
            out.push(UiCmd::ToggleColumn(c));
        }
    }
    ui.separator();
    if ui.button("Reset columns").clicked() {
        out.push(UiCmd::ResetColumns);
        ui.close();
    }
}

fn row_tint(p: &ProcessInfo, seq: u64) -> Option<Color32> {
    match p.state {
        ProcState::Exited => Some(theme::EXITED_ROW),
        _ if p.first_seen > 0 && seq.saturating_sub(p.first_seen) < HIGHLIGHT_TICKS => {
            Some(theme::NEW_ROW)
        }
        _ => None,
    }
}

fn text_color(p: &ProcessInfo, d: Option<&Arc<ProcessDetails>>) -> Color32 {
    if p.state == ProcState::Suspended {
        theme::SUSPENDED_TEXT
    } else if p.session_id == 0 && p.pid > 4 && !p.is_pseudo() {
        theme::SERVICE_TEXT
    } else if d.is_some_and(|d| d.elevated == Some(true)) {
        theme::ELEVATED_TEXT
    } else {
        theme::TEXT
    }
}

/// Tree lines: from each parent's toggle down to its children, ending at
/// the last one.
fn guides(ui: &Ui, r: Row) {
    if r.depth == 0 {
        return;
    }
    let cell = ui.max_rect();
    let painter = ui.painter();
    let stroke = Stroke::new(1.0, theme::BORDER_STRONG);
    // A level's line runs under the toggle of the parent at that depth.
    let x_at =
        |depth: u16| painter.round_to_pixel_center(cell.left() + (depth - 1) as f32 * INDENT + 6.0);
    // One past the row on both ends so consecutive rows join up.
    let (top, bottom, mid) = (cell.top() - 1.0, cell.bottom() + 1.0, cell.center().y);
    for depth in 1..r.depth.min(64) {
        if r.rails & (1 << depth) != 0 {
            painter.vline(x_at(depth), top..=bottom, stroke);
        }
    }
    let x = x_at(r.depth);
    painter.vline(x, top..=if r.last { mid } else { bottom }, stroke);
    let toggle = cell.left() + r.depth as f32 * INDENT;
    let end = toggle + if r.has_children { 1.0 } else { 11.0 };
    painter.hline(x..=end, painter.round_to_pixel_center(mid), stroke);
}

fn name_cell(
    ui: &mut Ui,
    p: &ProcessInfo,
    d: Option<&Arc<ProcessDetails>>,
    r: Row,
    group: Option<&Group>,
    icons: &mut IconCache,
    out: &mut Vec<UiCmd>,
) {
    ui.spacing_mut().item_spacing.x = 4.0;
    guides(ui, r);
    ui.add_space(r.depth as f32 * INDENT);
    let (rect, toggle) = ui.allocate_exact_size(vec2(12.0, ROW_HEIGHT), Sense::click());
    if r.has_children {
        let c = rect.center();
        let color = if toggle.hovered() {
            theme::TEXT
        } else {
            theme::TEXT_DIM
        };
        let points = if r.expanded {
            vec![
                c + vec2(-4.0, -2.0),
                c + vec2(4.0, -2.0),
                c + vec2(0.0, 3.0),
            ]
        } else {
            vec![
                c + vec2(-2.0, -4.0),
                c + vec2(3.0, 0.0),
                c + vec2(-2.0, 4.0),
            ]
        };
        ui.painter()
            .add(Shape::convex_polygon(points, color, Stroke::NONE));
        if toggle.clicked() {
            out.push(match group {
                Some(g) => UiCmd::ToggleGroup(g.id.clone()),
                None => UiCmd::ToggleCollapse(p.key),
            });
        }
    }

    match d.and_then(|d| d.icon.as_ref()) {
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
                egui::StrokeKind::Inside,
            );
        }
    }

    if p.not_responding {
        ui.label(widgets::icon(theme::icon::WARNING, 12.0).color(theme::BAD))
            .on_hover_text(
                "Not responding: a window of it has taken no message for 5 seconds. \
                 Wait chain (in the details) shows what it is waiting for.",
            );
    }
    let mut text = RichText::new(&*p.name).color(text_color(p, d));
    if r.dim || p.ending {
        text = text.color(theme::TEXT_DIM);
    }
    if p.state == ProcState::Suspended {
        text = text.italics();
    }
    ui.add(Label::new(text).truncate());
    // Gone in all but name: say so, the details say why.
    if p.ending || (p.threads == 0 && !p.is_pseudo()) {
        let (label, tip) = if p.ending {
            (
                "ending",
                "Windows is ending it but cannot finish: something still holds it. \
                 The details say what.",
            )
        } else {
            (
                "no threads",
                "It has no threads, so it does nothing. The details say more.",
            )
        };
        ui.label(
            RichText::new(label)
                .size(theme::META)
                .color(theme::ELEVATED_TEXT),
        )
        .on_hover_text(tip);
    }
    if let Some(g) = group {
        ui.label(RichText::new(format!("({})", g.members.len())).color(theme::TEXT_WEAK));
    }
}

fn name_tooltip(ui: &mut Ui, p: &ProcessInfo, d: &ProcessDetails) {
    ui.set_max_width(520.0);
    if let Some(desc) = &d.description {
        ui.label(widgets::strong(desc, theme::BODY));
    }
    if let Some(company) = &d.company {
        ui.label(RichText::new(company).color(theme::TEXT_WEAK));
    }
    if let Some(path) = &d.path {
        ui.label(path);
    }
    if let Some(cmd) = &d.command_line {
        ui.separator();
        ui.label(RichText::new(cmd).monospace().size(theme::META));
    }
    if p.state == ProcState::Suspended {
        ui.label(RichText::new("Suspended").color(theme::SUSPENDED_TEXT));
    }
}

#[allow(clippy::too_many_arguments)]
fn value_cell(
    ui: &mut Ui,
    c: Col,
    p: &ProcessInfo,
    d: Option<&Arc<ProcessDetails>>,
    is_group: bool,
    selected: bool,
    ctx: &Ctx<'_>,
) {
    let heat = cell_heat(c, p, ctx);
    // Only real load shows; the idle process "uses" idle time and the
    // selection carries its own highlight.
    if heat > 0.15 && p.state != ProcState::Exited && p.pid != 0 && !selected {
        ui.painter().rect_filled(
            ui.max_rect().shrink2(vec2(0.0, 1.0)),
            0.0,
            theme::heat(heat),
        );
    }

    // Blank means zero; data that needs elevation says so instead.
    if c == Col::Disk && ctx.snap.info_level != InfoLevel::Full && !p.is_pseudo() {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(widgets::icon(theme::icon::LOCK, 10.0).color(theme::TEXT_DIM))
                .on_hover_text("Disk counters need administrator rights");
        });
        return;
    }
    let text = cell_text(c, p, d, is_group, ctx);
    if text.is_empty() {
        if let Some(why) = unreadable(c, p, d, is_group) {
            ui.add_space(TEXT_INSET);
            ui.label(RichText::new("—").color(theme::TEXT_DIM))
                .on_hover_text(format!(
                    "Not readable: {why}. Running Anabolic as administrator reads most of these."
                ));
        }
        return;
    }
    let color = if p.state == ProcState::Suspended {
        theme::SUSPENDED_TEXT
    } else {
        theme::TEXT
    };
    let label = Label::new(RichText::new(text).color(color)).truncate();
    if c.numeric() {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| ui.add(label));
    } else {
        // Clear of the right-aligned number in the column before.
        ui.add_space(TEXT_INSET);
        ui.add(label);
    }
}

/// Why a text cell is empty because Windows refused to open the process,
/// rather than because there is nothing to show.
fn unreadable<'a>(
    c: Col,
    p: &ProcessInfo,
    d: Option<&'a Arc<ProcessDetails>>,
    is_group: bool,
) -> Option<&'a str> {
    if is_group || p.is_pseudo() {
        return None;
    }
    let d = d?;
    let why = d.open_error.as_deref()?;
    let missing = match c {
        Col::User => d.user.is_none(),
        Col::Integrity => d.integrity.is_none(),
        Col::Arch => d.arch.is_none(),
        Col::Path | Col::Description | Col::Company | Col::Version => d.path.is_none(),
        Col::CommandLine => d.command_line.is_none(),
        _ => false,
    };
    missing.then_some(why)
}

/// How hot a cell is, 0..=1. Linear against what makes a machine struggle,
/// so peaks stand out instead of every busy row: a quarter of all CPU time,
/// a quarter of RAM, 1 GB/s of transfer.
fn cell_heat(c: Col, p: &ProcessInfo, ctx: &Ctx<'_>) -> f32 {
    let memory = |bytes: u64| (bytes as f64 / (ctx.total_memory / 4.0)) as f32;
    let heat = match c {
        Col::Cpu => {
            let share = p.cpu / 25.0;
            // A process that keeps a whole core busy is worth a glance even
            // on a machine with many of them.
            if p.cpu * ctx.cores / 100.0 >= 0.9 {
                share.max(0.35)
            } else {
                share
            }
        }
        Col::Private => memory(p.private_bytes),
        Col::WorkingSet => memory(p.working_set),
        Col::PrivateWs => memory(p.private_working_set),
        Col::Disk => rate_heat(p.disk_total_rate()),
        Col::Io => rate_heat(p.io_total_rate()),
        Col::Gpu => p.gpu / 100.0,
        Col::Net => rate_heat(p.net_total_rate()),
        Col::HandleTrend => p.handles_per_min / 1000.0,
        _ => 0.0,
    };
    heat.clamp(0.0, 1.0)
}

/// 1 MB/s maps to nothing, 1 GB/s to full heat, logarithmic in between.
fn rate_heat(rate: f64) -> f32 {
    const MB: f64 = 1024.0 * 1024.0;
    if rate < MB {
        0.0
    } else {
        ((rate / MB).log10() / 3.0).min(1.0) as f32
    }
}

fn cell_text(
    c: Col,
    p: &ProcessInfo,
    d: Option<&Arc<ProcessDetails>>,
    is_group: bool,
    ctx: &Ctx<'_>,
) -> String {
    let pseudo = p.is_pseudo();
    let text = |f: fn(&ProcessDetails) -> Option<&str>| {
        d.and_then(|d| f(d)).unwrap_or_default().to_string()
    };
    match c {
        Col::Name => p.name.to_string(),
        Col::Pid | Col::Session | Col::Priority if is_group => String::new(),
        Col::Pid if p.pid == anabolic_core::INTERRUPTS_PID => String::new(),
        Col::Pid => p.pid.to_string(),
        Col::Cpu => fmt::cpu(p.cpu),
        Col::CpuTime if pseudo && p.pid != 0 => String::new(),
        Col::CpuTime => fmt::cpu_time(p.cpu_time),
        Col::Private if p.private_bytes > 0 => fmt::bytes(p.private_bytes),
        Col::WorkingSet if p.working_set > 0 => fmt::bytes(p.working_set),
        Col::PrivateWs if p.private_working_set > 0 => fmt::bytes(p.private_working_set),
        Col::Private | Col::WorkingSet | Col::PrivateWs => String::new(),
        Col::Disk if ctx.snap.info_level == InfoLevel::Full => fmt::rate(p.disk_total_rate()),
        Col::Disk => String::new(),
        Col::Io => fmt::rate(p.io_total_rate()),
        Col::Gpu => fmt::cpu(p.gpu),
        Col::Net => fmt::rate(p.net_total_rate()),
        Col::Threads | Col::Handles | Col::HandleTrend if pseudo => String::new(),
        Col::Threads => fmt::count(p.threads as u64),
        Col::Handles => fmt::count(p.handles as u64),
        Col::HandleTrend if p.handles_per_min.abs() < 0.5 => String::new(),
        Col::HandleTrend => format!("{:+.0}", p.handles_per_min),
        Col::Priority if pseudo => String::new(),
        Col::Priority => Priority::from_base(p.base_priority)
            .map_or_else(|| p.base_priority.to_string(), |pr| pr.label().into()),
        Col::Session if pseudo => String::new(),
        Col::Session => p.session_id.to_string(),
        Col::User => text(|d| d.user.as_deref().map(fmt::account)),
        Col::Integrity => d
            .and_then(|d| d.integrity)
            .map(|i| i.label().to_string())
            .unwrap_or_default(),
        Col::Arch => d
            .and_then(|d| d.arch)
            .map(|a| a.label().to_string())
            .unwrap_or_default(),
        Col::Description => text(|d| d.description.as_deref()),
        Col::Company => text(|d| d.company.as_deref()),
        Col::Version => text(|d| d.version.as_deref()),
        Col::Path => text(|d| d.path.as_deref()),
        Col::CommandLine if is_group => String::new(),
        Col::CommandLine => text(|d| d.command_line.as_deref()),
    }
}

pub(crate) fn item(ui: &mut Ui, out: &mut Vec<UiCmd>, text: &str, shortcut: &str, cmd: UiCmd) {
    if ui
        .add(egui::Button::new(text).shortcut_text(shortcut))
        .clicked()
    {
        out.push(cmd);
        ui.close();
    }
}

pub fn context_menu(
    ui: &mut Ui,
    p: &ProcessInfo,
    d: Option<&Arc<ProcessDetails>>,
    blocked: &[String],
    out: &mut Vec<UiCmd>,
) {
    ui.set_min_width(220.0);
    ui.label(RichText::new(format!("{}  ·  {}", p.name, p.pid)).color(theme::TEXT_WEAK));
    ui.separator();
    if p.is_pseudo() || p.state == ProcState::Exited {
        ui.label(RichText::new("Nothing to do here").color(theme::TEXT_DIM));
        return;
    }
    item(
        ui,
        out,
        "Terminate",
        "Del",
        UiCmd::Kill {
            key: p.key,
            tree: false,
        },
    );
    item(
        ui,
        out,
        "Terminate tree",
        "Shift+Del",
        UiCmd::Kill {
            key: p.key,
            tree: true,
        },
    );
    ui.separator();
    if p.state == ProcState::Suspended {
        item(ui, out, "Resume", "", UiCmd::Resume(p.key));
    } else {
        item(ui, out, "Suspend", "", UiCmd::Suspend(p.key));
    }
    let wait_label = if p.not_responding {
        "Why is it not responding?"
    } else {
        "Analyze wait chain"
    };
    item(ui, out, wait_label, "", UiCmd::ConfirmWaitChain(p.key));
    item(
        ui,
        out,
        &format!("Create a rule for {}\u{2026}", p.name),
        "",
        UiCmd::NewRule {
            image: p.name.clone(),
            base_priority: p.base_priority,
        },
    );
    let current = Priority::from_base(p.base_priority);
    ui.menu_button("Priority", |ui| {
        for pr in Priority::ALL {
            if ui.radio(current == Some(pr), pr.label()).clicked() {
                out.push(UiCmd::SetPriority(p.key, pr));
                ui.close();
            }
        }
    });
    let efficient = current == Some(Priority::Idle);
    item(
        ui,
        out,
        if efficient {
            "Leave efficiency mode"
        } else {
            "Efficiency mode"
        },
        "",
        UiCmd::Efficiency(p.key, !efficient),
    );
    item(ui, out, "Affinity…", "", UiCmd::AffinityDialog(p.key));
    item(
        ui,
        out,
        "Inspect handles",
        "",
        UiCmd::InspectHandles {
            key: p.key,
            names: false,
        },
    );
    item(ui, out, "Live activity", "", UiCmd::LiveActivity(p.key));
    if !anabolic_core::dump::refused(&p.name) {
        ui.menu_button("Create dump file", |ui| {
            item(
                ui,
                out,
                "Full: all of its memory\u{2026}",
                "",
                UiCmd::ConfirmDump(p.key, anabolic_core::dump::DumpKind::Full),
            );
            item(
                ui,
                out,
                "Small: threads, stacks, modules\u{2026}",
                "",
                UiCmd::ConfirmDump(p.key, anabolic_core::dump::DumpKind::Small),
            );
        });
    }

    let path = d.and_then(|d| d.path.clone());
    if let Some(path) = &path {
        network_item(ui, p, path, blocked, out);
    }
    ui.separator();
    if let Some(path) = &path {
        item(
            ui,
            out,
            "Open file location",
            "",
            UiCmd::OpenLocation(path.clone()),
        );
        item(ui, out, "Properties", "", UiCmd::Properties(path.clone()));
    }
    item(
        ui,
        out,
        "Search online",
        "",
        UiCmd::SearchOnline(p.name.to_string()),
    );
    ui.menu_button("Copy", |ui| {
        let mut copy = |ui: &mut Ui, label: &str, value: Option<String>| {
            if let Some(value) = value
                && ui.button(label).clicked()
            {
                out.push(UiCmd::Copy(value));
                ui.close();
            }
        };
        copy(ui, "Name", Some(p.name.to_string()));
        copy(ui, "PID", Some(p.pid.to_string()));
        copy(ui, "Path", path.clone());
        copy(ui, "Command line", d.and_then(|d| d.command_line.clone()));
    });
}

/// Windows' own files serve many programs at once (svchost.exe runs half
/// of Windows' services): blocking one cuts Windows off.
pub fn system_file(path: &str) -> bool {
    let windows = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    path.to_lowercase()
        .starts_with(&format!("{}\\", windows.to_lowercase()))
}

fn network_item(
    ui: &mut Ui,
    p: &ProcessInfo,
    path: &str,
    blocked: &[String],
    out: &mut Vec<UiCmd>,
) {
    if blocked.iter().any(|b| b.eq_ignore_ascii_case(path)) {
        item(
            ui,
            out,
            "Allow network access again",
            "",
            UiCmd::Unblock(path.to_string()),
        );
    } else if system_file(path) {
        ui.add_enabled(false, egui::Button::new("Block network access\u{2026}"))
            .on_disabled_hover_text(
                "A Windows file: it serves many programs and Windows itself, so blocking it \
                 would cut Windows off the network.",
            );
    } else {
        item(
            ui,
            out,
            "Block network access\u{2026}",
            "",
            UiCmd::ConfirmBlock {
                path: path.to_string(),
                name: p.name.clone(),
            },
        );
    }
}

fn group_menu(ui: &mut Ui, g: &Group, d: Option<&Arc<ProcessDetails>>, out: &mut Vec<UiCmd>) {
    ui.set_min_width(220.0);
    ui.label(
        RichText::new(format!("{}  ·  {} processes", g.agg.name, g.members.len()))
            .color(theme::TEXT_WEAK),
    );
    ui.separator();
    item(
        ui,
        out,
        &format!("Terminate all {}", g.members.len()),
        "Del",
        UiCmd::KillGroup(g.id.clone()),
    );
    item(
        ui,
        out,
        "Expand / collapse",
        "",
        UiCmd::ToggleGroup(g.id.clone()),
    );
    item(
        ui,
        out,
        &format!("Live activity of all {}", g.members.len()),
        "",
        UiCmd::LiveActivityGroup(g.id.clone()),
    );
    ui.separator();
    if let Some(path) = d.and_then(|d| d.path.clone()) {
        item(
            ui,
            out,
            "Open file location",
            "",
            UiCmd::OpenLocation(path.clone()),
        );
        item(ui, out, "Properties", "", UiCmd::Properties(path));
    }
    item(
        ui,
        out,
        "Search online",
        "",
        UiCmd::SearchOnline(g.agg.name.to_string()),
    );
    item(
        ui,
        out,
        "Copy name",
        "",
        UiCmd::Copy(g.agg.name.to_string()),
    );
}

// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Bottom panel: the selected process (identity, counters, threads, handles)
//! or the selected app group (totals and members).

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;

use anabolic_core::actions::Priority;
use anabolic_core::ending::EndReport;
use anabolic_core::handles::{HandleReport, ModuleReport, ModuleRow};
use anabolic_core::sysinfo::{local_time, now_filetime, thread_state_name, wait_reason_name};
use anabolic_core::waits::{Chain, Link, Status, WaitReport};
use anabolic_core::{ProcKey, ProcState, ProcessInfo, Snapshot, ThreadInfo};
use eframe::egui::{
    self, Align, Frame, Grid, Label, Layout, Margin, RichText, ScrollArea, Sense, Stroke, Ui, vec2,
};

use crate::fmt;
use crate::icons::IconCache;
use crate::panes::Panes;
use crate::proc_table::UiCmd;
use crate::table::{Columns, DetailTables, Spec, TableView, text_cmp};
use crate::theme;
use crate::tree::{Details, Group};
use crate::widgets;

mod end;
pub(crate) mod handles;
mod network;
mod waits;
use end::*;
use handles::*;
use network::*;
use waits::*;

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum DetailTab {
    #[default]
    General,
    Threads,
    Waits,
    Modules,
    Network,
    Handles,
}

/// Wait chains; reports arrive from the inspector.
#[derive(Default)]
pub struct WaitView {
    pub report: Option<Arc<WaitReport>>,
    /// An analysis is running for this process, since then.
    pub pending: Option<ProcKey>,
    pub started: Option<Instant>,
    /// Threads done, of how many.
    pub progress: Option<(usize, usize)>,
    pub error: Option<(ProcKey, String)>,
}

/// Why a process does not go away; reports arrive from the inspector.
#[derive(Default)]
pub struct EndView {
    /// The latest report, and when it arrived.
    pub report: Option<(Arc<EndReport>, Instant)>,
    pub pending: Option<ProcKey>,
    pub error: Option<(ProcKey, String)>,
    /// Processes Windows refused to end when asked.
    pub refused: HashSet<ProcKey>,
}

/// While the card is on screen, the report is read again this often: the
/// holders come and go.
const END_REFRESH: std::time::Duration = std::time::Duration::from_secs(10);

/// Modules of the selected process; also used to name thread start addresses.
#[derive(Default)]
pub struct ModuleView {
    pub report: Option<Arc<ModuleReport>>,
    pub pending: Option<ProcKey>,
    /// The module in the side pane, by base address.
    pub selected: Option<u64>,
}

impl ModuleView {
    fn for_key(&self, key: ProcKey) -> Option<&ModuleReport> {
        self.report.as_deref().filter(|r| r.key == key)
    }
}

/// `module!export+offset` (from the inspector) or `module+offset`.
fn symbol(report: Option<&ModuleReport>, address: u64) -> Option<String> {
    let report = report?;
    if let Some(name) = report.symbols.get(&address) {
        return Some(name.clone());
    }
    let i = report
        .modules
        .partition_point(|m| m.info.base <= address)
        .checked_sub(1)?;
    let m = &report.modules[i].info;
    (address < m.base + m.size).then(|| format!("{}+0x{:X}", m.name, address - m.base))
}

/// Handles tab state; reports arrive asynchronously from the inspector.
#[derive(Default)]
pub struct HandleView {
    pub report: Option<Arc<HandleReport>>,
    /// A request is in flight for this process.
    pub pending: Option<ProcKey>,
    pub error: Option<String>,
    type_filter: Option<u16>,
    by_name: bool,
    /// The handle in the side pane, by value.
    pub selected: Option<u64>,
    /// Sort and filter of the handle list; `groups` sorts the name groups.
    /// Both are applied in `refilter`, not every frame: a process can hold
    /// hundreds of thousands of handles.
    table: TableView,
    groups: TableView,
    /// Entry indices passing the filters, in order.
    rows: Vec<u32>,
    /// Indices into `name_groups` passing the filters, in order.
    group_rows: Vec<u32>,
}

impl HandleView {
    pub fn set_report(&mut self, report: Arc<HandleReport>) {
        if self.report.as_ref().is_none_or(|r| r.key != report.key) {
            self.selected = None;
        }
        self.pending = None;
        self.error = None;
        self.by_name = report.resolved > 0;
        if self
            .type_filter
            .is_some_and(|t| !report.by_type.iter().any(|c| c.type_index == t))
        {
            self.type_filter = None;
        }
        self.report = Some(report);
        self.refilter();
    }

    /// The command line's filter and order for the list of handles.
    pub fn set_table(&mut self, filter: &str, sort: Option<(usize, bool)>) {
        self.table.set_filter(filter);
        if sort.is_some() {
            self.table.sort = sort;
        }
        self.refilter();
    }

    fn refilter(&mut self) {
        let Some(report) = &self.report else { return };
        let type_ok = |t: u16| self.type_filter.is_none_or(|f| f == t);
        let name = |i: usize| report.names.get(i).and_then(|n| n.as_deref()).unwrap_or("");
        let entry = |i: u32| &report.entries[i as usize];
        let mut rows: Vec<u32> = (0..report.entries.len() as u32)
            .filter(|&i| {
                let e = entry(i);
                type_ok(e.type_index)
                    && (!self.table.filtering()
                        || self.table.matches(&[
                            &handles::value_filter_text(e.value),
                            &report.type_name(e.type_index),
                            name(i as usize),
                        ]))
            })
            .collect();
        if let Some((column, descending)) = self.table.sort {
            match column {
                0 => rows.sort_by_key(|&i| entry(i).value),
                1 => rows
                    .sort_by_cached_key(|&i| report.type_name(entry(i).type_index).to_lowercase()),
                2 => rows.sort_by_key(|&i| entry(i).access),
                _ => rows.sort_by_cached_key(|&i| name(i as usize).to_lowercase()),
            }
            if descending {
                rows.reverse();
            }
        }
        self.rows = rows;

        let mut groups: Vec<u32> = (0..report.name_groups.len() as u32)
            .filter(|&i| {
                let (t, n, _) = &report.name_groups[i as usize];
                type_ok(*t) && self.table.matches(&[&report.type_name(*t), n])
            })
            .collect();
        if let Some((column, descending)) = self.groups.sort {
            let group = |i: u32| &report.name_groups[i as usize];
            match column {
                0 => groups.sort_by_key(|&i| group(i).2),
                1 => groups.sort_by_cached_key(|&i| report.type_name(group(i).0).to_lowercase()),
                _ => groups.sort_by_cached_key(|&i| group(i).1.to_lowercase()),
            }
            if descending {
                groups.reverse();
            }
        }
        self.group_rows = groups;
    }
}

/// The panel's close button, at its top right: true when clicked.
fn close_button(ui: &mut Ui) -> bool {
    ui.add(egui::Button::new(widgets::icon(theme::icon::CLOSE, 11.0)).frame(false))
        .on_hover_text("Close the details (Esc)")
        .clicked()
}

/// The panel's title row; true when its close button was clicked.
fn header(
    ui: &mut Ui,
    icon: Option<egui::TextureId>,
    title: &str,
    badge: Option<String>,
    sub: &str,
    right: impl FnOnce(&mut Ui),
) -> bool {
    let mut closed = false;
    ui.horizontal(|ui| {
        if let Some(id) = icon {
            ui.image((id, vec2(28.0, 28.0)));
        }
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                ui.label(widgets::strong(title, theme::SUBTITLE));
                if let Some(badge) = badge {
                    ui.label(RichText::new(badge).color(theme::TEXT_WEAK));
                }
            });
            if !sub.is_empty() {
                ui.label(RichText::new(sub).color(theme::TEXT_WEAK));
            }
        });
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            closed = close_button(ui);
            ui.add_space(6.0);
            right(ui);
        });
    });
    ui.separator();
    closed
}

#[allow(clippy::too_many_arguments)]
pub fn show(
    ui: &mut Ui,
    tab: &mut DetailTab,
    key: ProcKey,
    snap: &Snapshot,
    details: &Details,
    icons: &mut IconCache,
    handles: &mut HandleView,
    modules: &mut ModuleView,
    panes: &Panes,
    tables: &mut DetailTables,
    waits: &WaitView,
    ends: &EndView,
    focus_thread: &mut Option<(u32, bool)>,
    services: &[(Arc<str>, Arc<str>)],
    growth: &anabolic_core::growth::Growth,
    blocked: &[String],
    origin: &Origin,
    out: &mut Vec<UiCmd>,
) {
    let Some(p) = snap.find(key) else {
        ui.horizontal(|ui| {
            ui.label(RichText::new("The process has exited.").color(theme::TEXT_WEAK));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if close_button(ui) {
                    out.push(UiCmd::Select(None));
                }
            });
        });
        return;
    };
    let d = details.get(&key);
    let icon = d
        .and_then(|d| d.icon.as_ref())
        .map(|i| icons.texture(ui.ctx(), i));
    let mut badge = format!("PID {}", p.pid);
    match p.state {
        ProcState::Suspended => badge.push_str("  ·  SUSPENDED"),
        ProcState::Exited => badge.push_str("  ·  EXITED"),
        ProcState::Running => {}
    }
    if p.not_responding {
        badge.push_str("  ·  NOT RESPONDING");
    }
    if p.ending {
        badge.push_str("  ·  ENDING, STUCK");
    } else if p.threads == 0 && !p.is_pseudo() {
        badge.push_str("  ·  NO THREADS");
    }
    if d.is_some_and(|d| d.critical) {
        badge.push_str("  ·  CRITICAL");
    }
    let sub = [
        d.and_then(|d| d.description.clone()),
        d.and_then(|d| d.company.clone()),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join("  ·  ");
    let closed = header(ui, icon, &p.name, Some(badge), &sub, |ui| {
        ui.selectable_value(
            tab,
            DetailTab::Handles,
            format!("Handles ({})", fmt::count(p.handles as u64)),
        );
        let net = snap
            .selected
            .as_ref()
            .filter(|s| s.key == p.key)
            .map_or(0, |s| s.connections.len());
        ui.selectable_value(tab, DetailTab::Network, format!("Network ({net})"));
        ui.selectable_value(tab, DetailTab::Modules, "Modules");
        let waits_label: egui::WidgetText = if p.not_responding {
            let mut job = widgets::icon_text(ui, theme::icon::WARNING, "Wait chain");
            if let Some(section) = job.sections.first_mut() {
                section.format.color = theme::BAD;
            }
            job.into()
        } else {
            "Wait chain".into()
        };
        ui.selectable_value(tab, DetailTab::Waits, waits_label);
        ui.selectable_value(tab, DetailTab::Threads, format!("Threads ({})", p.threads));
        ui.selectable_value(tab, DetailTab::General, "General");
        // Right to left: these end up left of the tabs.
        // Nothing to end once it is ending, nothing to suspend without threads.
        if !p.is_pseudo() && p.state != ProcState::Exited && !p.ending {
            ui.separator();
            let terminate =
                egui::Button::new(widgets::icon_text(ui, theme::icon::CLOSE, "Terminate"));
            if ui
                .add(terminate)
                .on_hover_text("End the process (Del)")
                .clicked()
            {
                out.push(UiCmd::Kill {
                    key: p.key,
                    tree: false,
                });
            }
        }
        if !p.is_pseudo() && p.state != ProcState::Exited {
            let live =
                egui::Button::new(widgets::icon_text(ui, theme::icon::RECORD, "Live activity"));
            if ui
                .add(live)
                .on_hover_text(if cfg!(windows) {
                    "What it does from now on, step by step, on the Activity page: files, \
                     registry, network, programs started. In the tree view, with the processes \
                     under it."
                } else {
                    "What it does from now on, step by step, on the Activity page: files, \
                     network, programs started. In the tree view, with the processes under it."
                })
                .clicked()
            {
                out.push(UiCmd::LiveActivity(p.key));
            }
        }
        if !p.is_pseudo() && p.state != ProcState::Exited && p.threads > 0 {
            let (glyph, label, cmd): (_, _, fn(ProcKey) -> UiCmd) =
                if p.state == ProcState::Suspended {
                    (theme::icon::PLAY, "Resume", UiCmd::Resume)
                } else {
                    (theme::icon::PAUSE, "Suspend", UiCmd::Suspend)
                };
            if ui
                .add(egui::Button::new(widgets::icon_text(ui, glyph, label)))
                .clicked()
            {
                out.push(cmd(p.key));
            }
        }
    });
    if closed {
        out.push(UiCmd::Select(None));
    }

    match tab {
        DetailTab::General => {
            if needs_end_card(p, ends) {
                end_card(ui, p, snap, ends, out);
            }
            general(ui, p, snap, details, services, growth, origin, out)
        }
        DetailTab::Threads => threads(
            ui,
            p,
            snap,
            modules,
            panes,
            &mut tables.threads,
            focus_thread,
            out,
        ),
        DetailTab::Waits => waits_tab(ui, p, snap, waits, out),
        DetailTab::Modules => modules_tab(ui, p, modules, panes, &mut tables.modules, out),
        DetailTab::Handles => handles_tab(ui, p, snap, handles, panes, out),
        DetailTab::Network => {
            let path = details.get(&key).and_then(|d| d.path.as_deref());
            let blocked = path.is_some_and(|p| blocked.iter().any(|b| b.eq_ignore_ascii_case(p)));
            network_tab(ui, p, snap, tables, path, blocked, out)
        }
    }
}

/// What the "Started by" block reads: command lines kept after their
/// processes exit, and the services list (who a service host is).
pub struct Origin<'a> {
    pub cmdlines: &'a std::collections::HashMap<ProcKey, Arc<str>>,
    pub services: Option<&'a anabolic_core::services::ServiceList>,
}

#[allow(clippy::too_many_arguments)]
fn general(
    ui: &mut Ui,
    p: &ProcessInfo,
    snap: &Snapshot,
    details: &Details,
    services: &[(Arc<str>, Arc<str>)],
    growth: &anabolic_core::growth::Growth,
    origin: &Origin,
    out: &mut Vec<UiCmd>,
) {
    let d = details.get(&p.key);
    ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.columns(2, |cols| {
                let ui = &mut cols[0];
                Grid::new("identity")
                    .num_columns(2)
                    .spacing([12.0, 4.0])
                    .show(ui, |ui| {
                        let row = |ui: &mut Ui, key: &str, value: String| {
                            ui.label(RichText::new(key).color(theme::TEXT_WEAK));
                            ui.add(Label::new(value).wrap().selectable(true));
                            ui.end_row();
                        };
                        let dash = || "—".to_string();
                        row(
                            ui,
                            "Path",
                            d.and_then(|d| d.path.clone()).unwrap_or_else(dash),
                        );
                        row(
                            ui,
                            "Command line",
                            d.and_then(|d| d.command_line.clone()).unwrap_or_else(dash),
                        );
                        row(
                            ui,
                            "User",
                            d.and_then(|d| d.user.clone()).unwrap_or_else(dash),
                        );

                        ui.label(RichText::new("Parent").color(theme::TEXT_WEAK));
                        match parent_of(p, snap) {
                            Some(parent) => {
                                if ui
                                    .link(format!("{} ({})", parent.name, parent.pid))
                                    .clicked()
                                {
                                    out.push(UiCmd::Select(Some(parent.key)));
                                }
                            }
                            None => {
                                // Gone: named when this session saw it.
                                let known = snap
                                    .selected
                                    .as_ref()
                                    .filter(|s| s.key == p.key)
                                    .and_then(|s| s.ancestry.first());
                                let text = match known {
                                    Some(a) => format!("{} ({}), exited", a.name, a.key.pid),
                                    None => format!("exited ({})", p.ppid),
                                };
                                ui.label(RichText::new(text).color(theme::TEXT_WEAK));
                            }
                        }
                        ui.end_row();

                        let started = local_time(p.key.create_time).map(|t| {
                            let age = (now_filetime() - p.key.create_time).max(0) as u64;
                            format!(
                                "{t}  (up {})",
                                fmt::duration(std::time::Duration::from_nanos(age * 100))
                            )
                        });
                        row(ui, "Started", started.unwrap_or_else(dash));
                        #[cfg(windows)]
                        let security = match d {
                            Some(d) => format!(
                                "{}{}  ·  {}",
                                d.integrity.map_or("?", |i| i.label()),
                                if d.elevated == Some(true) {
                                    ", elevated"
                                } else {
                                    ""
                                },
                                d.arch.map_or("?", |a| a.label())
                            ),
                            None => dash(),
                        };
                        #[cfg(windows)]
                        row(ui, "Integrity", security);
                        // Linux has no integrity levels: root (or extra
                        // capabilities) or an ordinary user's rights.
                        #[cfg(target_os = "linux")]
                        row(
                            ui,
                            "Rights",
                            match d {
                                Some(d) => format!(
                                    "{}  ·  {}",
                                    match d.elevated {
                                        Some(true) => "root or capabilities",
                                        Some(false) => "a user's",
                                        None => "?",
                                    },
                                    d.arch.map_or("?", |a| a.label())
                                ),
                                None => dash(),
                            },
                        );
                        row(
                            ui,
                            "Priority",
                            format!(
                                "{} (base {})  ·  session {}",
                                Priority::from_base(p.base_priority)
                                    .map_or("Custom", |x| x.label()),
                                p.base_priority,
                                p.session_id
                            ),
                        );
                        if let Some(e) = d.and_then(|d| d.open_error.as_ref()) {
                            row(ui, "Access", e.clone());
                        }
                        if !services.is_empty() {
                            ui.label(RichText::new("Services").color(theme::TEXT_WEAK));
                            ui.horizontal_wrapped(|ui| {
                                for (i, (name, display)) in services.iter().enumerate() {
                                    if i > 0 {
                                        ui.label(RichText::new("·").color(theme::TEXT_DIM));
                                    }
                                    if widgets::link(ui, display)
                                        .on_hover_text(format!(
                                            "{name}: open it on the Services page"
                                        ))
                                        .clicked()
                                    {
                                        out.push(UiCmd::SelectService(name.clone()));
                                    }
                                }
                                if services.len() > 1
                                    && ui.small_button("All on the Services page").clicked()
                                {
                                    out.push(UiCmd::ShowServices(p.pid));
                                }
                            });
                            ui.end_row();
                        }
                    });
                crate::origin::show(ui, p, snap, origin.cmdlines, origin.services, out);

                let ui = &mut cols[1];
                counters(ui, p);
                let leaks: Vec<_> = growth.leaks_of(p.key).collect();
                if !leaks.is_empty() {
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        ui.label(
                            widgets::icon(theme::icon::WARNING, 12.0).color(theme::ELEVATED_TEXT),
                        );
                        ui.label(
                            widgets::strong("Growing steadily", theme::BODY)
                                .color(theme::ELEVATED_TEXT),
                        );
                    });
                    for l in leaks {
                        ui.label(RichText::new(fmt::leak(l)).color(theme::TEXT_WEAK));
                    }
                    ui.label(
                        RichText::new(
                            "A straight climb that has not levelled off: usually a leak. \
                             Restarting the program gives it back.",
                        )
                        .size(theme::META)
                        .color(theme::TEXT_DIM),
                    );
                }
            });
        });
}

fn counters(ui: &mut Ui, p: &ProcessInfo) {
    // Fixed columns: the values change every refresh.
    let pair = |ui: &mut Ui, k1: &str, v1: String, k2: &str, v2: String| {
        ui.horizontal(|ui| {
            widgets::text(
                ui,
                100.0,
                Align::Min,
                RichText::new(k1).color(theme::TEXT_WEAK),
            );
            widgets::text(ui, 120.0, Align::Min, v1);
            widgets::text(
                ui,
                110.0,
                Align::Min,
                RichText::new(k2).color(theme::TEXT_WEAK),
            );
            widgets::text(ui, 160.0, Align::Min, v2);
        });
    };
    pair(
        ui,
        "CPU",
        format!("{:.2} %", p.cpu),
        "Kernel",
        format!("{:.2} %", p.cpu_kernel),
    );
    pair(
        ui,
        "CPU time",
        fmt::cpu_time(p.cpu_time),
        "Cycles/s",
        fmt::count(p.cycles_delta),
    );
    pair(
        ui,
        "Memory",
        fmt::bytes(p.private_working_set),
        "Private bytes",
        fmt::bytes(p.private_bytes),
    );
    pair(
        ui,
        "Working set",
        fmt::bytes(p.working_set),
        "Peak WS",
        fmt::bytes(p.peak_working_set),
    );
    pair(
        ui,
        "Virtual",
        fmt::bytes(p.virtual_size),
        "Paged / NP pool",
        format!(
            "{} / {}",
            fmt::bytes(p.paged_pool),
            fmt::bytes(p.nonpaged_pool)
        ),
    );
    pair(
        ui,
        "Page faults",
        fmt::count(p.page_faults as u64),
        "Faults/s",
        fmt::count(p.page_faults_delta as u64),
    );
    pair(
        ui,
        "Hard faults",
        fmt::count(p.hard_faults as u64),
        "Ctx switches/s",
        fmt::count(p.context_switches_delta),
    );
    pair(
        ui,
        "I/O read",
        fmt::rate_always(p.io_read_rate),
        "total",
        fmt::bytes(p.io.read_bytes),
    );
    pair(
        ui,
        "I/O write",
        fmt::rate_always(p.io_write_rate),
        "total",
        fmt::bytes(p.io.write_bytes),
    );
    pair(
        ui,
        "I/O other",
        fmt::rate_always(p.io_other_rate),
        "total",
        fmt::bytes(p.io.other_bytes),
    );
    if let Some(disk) = p.disk {
        pair(
            ui,
            "Disk read",
            fmt::rate_always(p.disk_read_rate),
            "total",
            fmt::bytes(disk.read_bytes),
        );
        pair(
            ui,
            "Disk write",
            fmt::rate_always(p.disk_write_rate),
            "total",
            fmt::bytes(disk.write_bytes),
        );
    }
    pair(
        ui,
        "Threads",
        fmt::count(p.threads as u64),
        "Handles",
        format!(
            "{}  ({:+.0}/min)",
            fmt::count(p.handles as u64),
            p.handles_per_min
        ),
    );
}

fn parent_of<'a>(p: &ProcessInfo, snap: &'a Snapshot) -> Option<&'a ProcessInfo> {
    snap.processes
        .iter()
        .filter(|q| q.pid == p.ppid && q.key != p.key && q.key.create_time <= p.key.create_time)
        .max_by_key(|q| q.key.create_time)
}

fn right(ui: &mut Ui, text: impl Into<egui::WidgetText>) {
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| ui.label(text));
}

/// A process by PID, as a link to it when it is another one.
fn process_link(ui: &mut Ui, pid: u32, own: u32, snap: &Snapshot, out: &mut Vec<UiCmd>) {
    let live = snap
        .processes
        .iter()
        .find(|q| q.pid == pid && q.state != ProcState::Exited);
    match live {
        Some(q) if pid != own => {
            if widgets::link(ui, &format!("{} ({pid})", q.name))
                .on_hover_text("Show it in the process list")
                .clicked()
            {
                out.push(UiCmd::GoTo(q.key));
            }
        }
        Some(q) => {
            ui.label(format!("{} ({pid})", q.name));
        }
        None => {
            ui.label(RichText::new(format!("PID {pid}")).color(theme::TEXT_WEAK));
        }
    }
}

/// Asks for the module list: automatically for System (only kernel
/// structures are read), behind a confirmation for everything else.
fn module_loader(
    ui: &mut Ui,
    p: &ProcessInfo,
    modules: &ModuleView,
    label: &str,
    out: &mut Vec<UiCmd>,
) {
    if modules.for_key(p.key).is_some() {
        return;
    }
    if modules.pending == Some(p.key) {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(RichText::new("Reading modules…").color(theme::TEXT_WEAK));
        });
    } else if p.pid == anabolic_core::scan::KERNEL_PID {
        out.push(UiCmd::LoadModules(p.key));
    } else if ui
        .button(label)
        .on_hover_text("Reads the module list out of the process (opens it for reading).")
        .clicked()
    {
        out.push(UiCmd::ConfirmLoadModules(p.key));
    }
}

/// One row of the threads table, with what sorting and filtering read.
struct ThreadRow<'a> {
    t: &'a ThreadInfo,
    tid: String,
    name: &'a str,
    start: String,
    wait: &'static str,
}

/// `focus`: the selected thread, and whether it was scrolled to already.
#[allow(clippy::too_many_arguments)]
fn threads(
    ui: &mut Ui,
    p: &ProcessInfo,
    snap: &Snapshot,
    modules: &ModuleView,
    panes: &Panes,
    view: &mut TableView,
    focus: &mut Option<(u32, bool)>,
    out: &mut Vec<UiCmd>,
) {
    let Some(sel) = snap.selected.as_ref().filter(|s| s.key == p.key) else {
        ui.label(RichText::new("Collecting…").color(theme::TEXT_WEAK));
        return;
    };
    let report = modules.for_key(p.key);
    let start_of = |t: &ThreadInfo| {
        if t.win32_start_address != 0 {
            t.win32_start_address
        } else {
            t.start_address
        }
    };
    let start_name = |t: &ThreadInfo| {
        let address = start_of(t);
        symbol(report, address).unwrap_or_else(|| format!("0x{address:X}"))
    };
    let names = report.map(|r| &r.thread_names);
    let mut list: Vec<ThreadRow> = sel
        .threads
        .iter()
        .map(|t| ThreadRow {
            t,
            tid: t.tid.to_string(),
            name: names.and_then(|n| n.get(&t.tid)).map_or("", |s| s.as_str()),
            start: start_name(t),
            wait: if t.state == 5 {
                wait_reason_name(t.wait_reason)
            } else {
                ""
            },
        })
        .filter(|r| {
            view.matches(&[
                &r.tid,
                r.name,
                &r.start,
                thread_state_name(r.t.state),
                r.wait,
            ])
        })
        .collect();
    view.order(&mut list, |a, b, column| {
        let (x, y) = (a.t, b.t);
        match column {
            0 => x.tid.cmp(&y.tid),
            1 => x.cpu.total_cmp(&y.cpu),
            2 => (x.kernel_time + x.user_time).cmp(&(y.kernel_time + y.user_time)),
            3 => text_cmp(a.name, b.name),
            4 => text_cmp(&a.start, &b.start),
            5 => x.priority.cmp(&y.priority),
            6 => x.context_switches_delta.cmp(&y.context_switches_delta),
            7 => thread_state_name(x.state).cmp(thread_state_name(y.state)),
            _ => a.wait.cmp(b.wait),
        }
        .then(x.tid.cmp(&y.tid))
    });
    ui.horizontal(|ui| {
        if view.filter_box(
            ui,
            "Filter: TID, name, start, state",
            list.len(),
            sel.threads.len(),
        ) {
            ui.ctx().request_repaint();
        }
        module_loader(ui, p, modules, "Resolve start addresses and names", out);
    });

    let focus_tid = focus.map(|(tid, _)| tid);
    if let Some(r) = focus_tid.and_then(|tid| list.iter().find(|r| r.t.tid == tid)) {
        crate::panes::side(ui, ("thread-pane", p.key), |ui| {
            crate::panes::thread_pane(ui, p, r.t, report, &r.start, start_of(r.t), panes, out)
        });
    }
    egui::CentralPanel::no_frame().show(ui, |ui| {
        const TITLES: [(&str, bool); 9] = [
            ("TID", true),
            ("CPU", true),
            ("CPU time", true),
            ("Name", false),
            ("Start address", false),
            ("Priority", true),
            ("Ctx sw/s", true),
            ("State", false),
            ("Wait reason", false),
        ];
        const COLUMNS: [Spec; 9] = [
            Spec::new(70.0),
            Spec::new(60.0),
            Spec::new(100.0),
            Spec::new(150.0).clip(),
            Spec::new(280.0).clip(),
            Spec::new(70.0),
            Spec::new(90.0),
            Spec::new(90.0),
            Spec::new(160.0),
        ];
        Columns::load(ui.ctx(), "threads", &COLUMNS).show(ui, |mut table, columns| {
            if let Some((tid, scrolled)) = focus
                && !*scrolled
                && let Some(row) = list.iter().position(|r| r.t.tid == *tid)
            {
                table = table.scroll_to_row(row, Some(Align::Center));
                *scrolled = true;
            }
            table
                .striped(true)
                .resizable(true)
                .auto_shrink([false, false])
                .sense(Sense::click())
                .cell_layout(Layout::left_to_right(Align::Center))
                .header(20.0, |mut h| {
                    columns.titles(&mut h, |ui, c| {
                        let (title, numeric) = TITLES[c];
                        view.header(ui, c, title, numeric);
                    });
                })
                .body(|body| {
                    columns.keep(body.widths());
                    body.rows(18.0, list.len(), |mut row| {
                        let r = &list[row.index()];
                        let t = r.t;
                        row.set_selected(focus_tid == Some(t.tid));
                        for &c in columns.order() {
                            row.col(|ui| match c {
                                0 => right(ui, r.tid.clone()),
                                1 => right(ui, fmt::cpu(t.cpu)),
                                2 => right(ui, fmt::cpu_time(t.kernel_time + t.user_time)),
                                3 => {
                                    ui.add(Label::new(r.name).truncate());
                                }
                                4 => {
                                    let address = start_of(t);
                                    let known = report.is_some() && !r.start.starts_with("0x");
                                    let text = RichText::new(&r.start).monospace();
                                    let text = if known {
                                        text
                                    } else {
                                        text.color(theme::TEXT_WEAK)
                                    };
                                    ui.add(Label::new(text).truncate())
                                        .on_hover_text(format!("0x{address:X}"));
                                }
                                5 => right(ui, format!("{} / {}", t.priority, t.base_priority)),
                                6 => right(ui, fmt::count(t.context_switches_delta as u64)),
                                7 => {
                                    ui.label(thread_state_name(t.state));
                                }
                                _ => {
                                    ui.label(RichText::new(r.wait).color(theme::TEXT_WEAK));
                                }
                            });
                        }
                        let response = row.response();
                        if response.clicked() {
                            *focus = if focus_tid == Some(t.tid) {
                                None
                            } else {
                                Some((t.tid, true))
                            };
                        }
                        widgets::row_menu(&response, "threads", t.tid, |ui| {
                            crate::panes::thread_menu(ui, p, t, &r.start, out);
                        });
                    });
                })
        });
    });
}

/// Summary for an app group (Apps view).
pub fn show_group(
    ui: &mut Ui,
    group: &Group,
    snap: &Snapshot,
    details: &Details,
    icons: &mut IconCache,
    out: &mut Vec<UiCmd>,
) {
    let first = group.members.first().map(|&i| &snap.processes[i as usize]);
    let d = first.and_then(|f| details.get(&f.key));
    let icon = d
        .and_then(|d| d.icon.as_ref())
        .map(|i| icons.texture(ui.ctx(), i));
    let sub = [
        d.and_then(|d| d.description.clone()),
        d.and_then(|d| d.company.clone()),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join("  ·  ");
    let closed = header(
        ui,
        icon,
        &group.agg.name,
        Some(format!("{} processes", group.members.len())),
        &sub,
        |ui| {
            if ui
                .button(
                    RichText::new(format!("Terminate all {}", group.members.len()))
                        .color(theme::BAD),
                )
                .clicked()
            {
                out.push(UiCmd::KillGroup(group.id.clone()));
            }
            // Right to left: left of the button above.
            if ui
                .add(egui::Button::new(widgets::icon_text(
                    ui,
                    theme::icon::RECORD,
                    "Live activity",
                )))
                .on_hover_text(format!(
                    "What every {} does from now on, step by step, on the Activity page",
                    group.agg.name
                ))
                .clicked()
            {
                out.push(UiCmd::LiveActivityGroup(group.id.clone()));
            }
        },
    );
    if closed {
        out.push(UiCmd::Select(None));
    }

    ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.columns(2, |cols| {
                let ui = &mut cols[0];
                ui.label(RichText::new("Members by memory").color(theme::TEXT_WEAK));
                let mut members: Vec<&ProcessInfo> = group
                    .members
                    .iter()
                    .map(|&i| &snap.processes[i as usize])
                    .filter(|p| p.state != ProcState::Exited)
                    .collect();
                members.sort_by_key(|p| std::cmp::Reverse(p.private_working_set));
                widgets::header(
                    ui,
                    &[
                        ("PID", 70.0, Align::Min),
                        ("Memory", 90.0, Align::Max),
                        ("CPU", 60.0, Align::Max),
                        ("Kind", 260.0, Align::Min),
                    ],
                );
                for (i, p) in members.iter().take(40).enumerate() {
                    widgets::row(ui, i, |ui| {
                        widgets::cell(ui, 70.0, Align::Min, |ui| {
                            if widgets::link(ui, &p.pid.to_string()).clicked() {
                                out.push(UiCmd::Select(Some(p.key)));
                            }
                        });
                        widgets::text(ui, 90.0, Align::Max, fmt::bytes(p.private_working_set));
                        widgets::text(
                            ui,
                            60.0,
                            Align::Max,
                            RichText::new(fmt::cpu(p.cpu)).color(theme::TEXT_WEAK),
                        );
                        let cmd = details
                            .get(&p.key)
                            .and_then(|d| d.command_line.as_deref())
                            .unwrap_or("");
                        widgets::text(
                            ui,
                            260.0,
                            Align::Min,
                            RichText::new(short_cmd(cmd)).small().color(theme::TEXT_DIM),
                        );
                    });
                }
                counters(&mut cols[1], &group.agg);
            });
        });
}

/// Chrome-style command lines are mostly flags; the --type= one says what
/// the process is.
fn short_cmd(cmd: &str) -> String {
    match cmd.split_whitespace().find(|a| a.starts_with("--type=")) {
        Some(kind) => kind.trim_start_matches("--type=").to_string(),
        None => cmd.chars().take(120).collect(),
    }
}

fn modules_tab(
    ui: &mut Ui,
    p: &ProcessInfo,
    modules: &mut ModuleView,
    panes: &Panes,
    view: &mut TableView,
    out: &mut Vec<UiCmd>,
) {
    let Some(report) = modules.report.clone().filter(|r| r.key == p.key) else {
        module_loader(ui, p, modules, "Load modules", out);
        return;
    };
    if let Some(e) = &report.error {
        ui.label(
            RichText::new(format!("Could not read the module list: {e}"))
                .color(theme::ELEVATED_TEXT),
        );
    }
    let text = |s: &Option<String>| s.clone().unwrap_or_default();
    let mut rows: Vec<&ModuleRow> = report
        .modules
        .iter()
        .filter(|m| {
            view.matches(&[
                &m.info.name,
                &m.info.path,
                &format!("{:X}", m.info.base),
                m.description.as_deref().unwrap_or(""),
                m.company.as_deref().unwrap_or(""),
                m.version.as_deref().unwrap_or(""),
            ])
        })
        .collect();
    view.order(&mut rows, |a, b, column| match column {
        0 => text_cmp(&a.info.name, &b.info.name),
        1 => a.info.base.cmp(&b.info.base),
        2 => a.info.size.cmp(&b.info.size),
        3 => text_cmp(&text(&a.description), &text(&b.description)),
        4 => text_cmp(&text(&a.company), &text(&b.company)),
        5 => text_cmp(&text(&a.version), &text(&b.version)),
        _ => text_cmp(&a.info.path, &b.info.path),
    });
    ui.horizontal(|ui| {
        if view.filter_box(
            ui,
            "Filter: name, path, company, version",
            rows.len(),
            report.modules.len(),
        ) {
            ui.ctx().request_repaint();
        }
        // While filtering, the box says how many there are.
        let count = if view.filtering() {
            String::new()
        } else {
            format!("{} modules  ·  ", report.modules.len())
        };
        ui.label(
            RichText::new(format!(
                "{count}{}{}  ·  click one for its signature and hash",
                fmt::millis(report.took),
                if p.pid == anabolic_core::scan::KERNEL_PID {
                    "  ·  kernel image and drivers"
                } else {
                    ""
                }
            ))
            .small()
            .color(theme::TEXT_WEAK),
        );
    });
    let selected = modules.selected;
    if let Some(m) = selected.and_then(|base| report.modules.iter().find(|m| m.info.base == base)) {
        crate::panes::side(ui, ("module-pane", p.key), |ui| {
            crate::panes::module_pane(ui, m, panes, out)
        });
    }
    egui::CentralPanel::no_frame().show(ui, |ui| {
        const TITLES: [(&str, bool); 7] = [
            ("Name", false),
            ("Base", false),
            ("Size", true),
            ("Description", false),
            (if cfg!(windows) { "Company" } else { "Package" }, false),
            ("Version", false),
            ("Path", false),
        ];
        const COLUMNS: [Spec; 7] = [
            Spec::new(170.0).clip(),
            Spec::new(150.0),
            Spec::new(80.0),
            Spec::new(220.0).clip(),
            Spec::new(150.0).clip(),
            Spec::new(110.0).clip(),
            Spec::new(320.0).clip(),
        ];
        Columns::load(ui.ctx(), "modules", &COLUMNS).show(ui, |table, columns| {
            table
                .striped(true)
                .resizable(true)
                .auto_shrink([false, false])
                .sense(Sense::click())
                .cell_layout(Layout::left_to_right(Align::Center))
                .header(20.0, |mut h| {
                    columns.titles(&mut h, |ui, c| {
                        let (title, numeric) = TITLES[c];
                        view.header(ui, c, title, numeric);
                    });
                })
                .body(|body| {
                    columns.keep(body.widths());
                    body.rows(18.0, rows.len(), |mut row| {
                        let m = rows[row.index()];
                        row.set_selected(selected == Some(m.info.base));
                        for &c in columns.order() {
                            row.col(|ui| match c {
                                0 => {
                                    ui.add(Label::new(&*m.info.name).truncate());
                                }
                                1 => {
                                    ui.label(
                                        RichText::new(format!("0x{:X}", m.info.base))
                                            .monospace()
                                            .color(theme::TEXT_WEAK),
                                    );
                                }
                                2 => right(ui, fmt::bytes(m.info.size)),
                                3 => {
                                    ui.add(
                                        Label::new(m.description.clone().unwrap_or_default())
                                            .truncate(),
                                    );
                                }
                                4 => {
                                    ui.add(
                                        Label::new(
                                            RichText::new(m.company.clone().unwrap_or_default())
                                                .color(theme::TEXT_WEAK),
                                        )
                                        .truncate(),
                                    );
                                }
                                5 => {
                                    ui.add(
                                        Label::new(
                                            RichText::new(m.version.clone().unwrap_or_default())
                                                .color(theme::TEXT_WEAK),
                                        )
                                        .truncate(),
                                    );
                                }
                                _ => {
                                    ui.add(
                                        Label::new(
                                            RichText::new(&*m.info.path)
                                                .small()
                                                .color(theme::TEXT_DIM),
                                        )
                                        .truncate(),
                                    );
                                }
                            });
                        }
                        let response = row.response();
                        if response.clicked() {
                            modules.selected = if selected == Some(m.info.base) {
                                None
                            } else {
                                Some(m.info.base)
                            };
                        }
                        response.context_menu(|ui| {
                            let facts = panes
                                .facts
                                .get(&m.info.path)
                                .and_then(|f| f.as_ref().ok())
                                .map(|f| &**f);
                            crate::panes::module_menu(ui, m, facts, out);
                        });
                    });
                })
        });
    });
}

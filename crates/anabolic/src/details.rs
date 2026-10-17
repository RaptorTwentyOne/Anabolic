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
use egui_extras::{Column, TableBuilder};

use crate::fmt;
use crate::icons::IconCache;
use crate::panes::Panes;
use crate::proc_table::UiCmd;
use crate::table::{DetailTables, TableView, text_cmp};
use crate::theme;
use crate::tree::{Details, Group};
use crate::widgets;

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

/// Whether the card has something to explain about `p`.
fn needs_end_card(p: &ProcessInfo, view: &EndView) -> bool {
    p.ending || (p.threads == 0 && !p.is_pseudo()) || view.refused.contains(&p.key)
}

/// Rights of Process handles, the ones worth naming.
fn process_access(access: u32) -> String {
    if access & 0x1F_FFFF == 0x1F_FFFF {
        return "full access".into();
    }
    let names: Vec<&str> = [
        (0x0001, "end"),
        (0x0002, "create threads"),
        (0x0020, "write memory"),
        (0x0010, "read memory"),
        (0x0040, "duplicate handles"),
        (0x0800, "suspend"),
        (0x0200, "set information"),
        (0x1400, "query"),
    ]
    .iter()
    .filter(|(bit, _)| access & bit != 0)
    .map(|(_, name)| *name)
    .collect();
    if names.is_empty() {
        format!("access 0x{access:X}")
    } else {
        names.join(", ")
    }
}

/// "It does not go away, and this is why", with what can be done.
fn end_card(ui: &mut Ui, p: &ProcessInfo, snap: &Snapshot, view: &EndView, out: &mut Vec<UiCmd>) {
    let report = view
        .report
        .as_ref()
        .filter(|(r, _)| r.key == p.key)
        .map(|(r, at)| (r.clone(), *at));
    let fresh = report
        .as_ref()
        .is_some_and(|(_, at)| at.elapsed() < END_REFRESH);
    if !fresh && view.pending != Some(p.key) {
        out.push(UiCmd::WhyAlive(p.key));
    }
    ui.ctx().request_repaint_after(END_REFRESH);
    let report = report.map(|(r, _)| r);

    let (title, color) = match &report {
        Some(r) if r.flags.deleting => ("Windows is ending it but cannot finish", theme::BAD),
        Some(r) if r.protection != 0 => ("Windows protects it", theme::ELEVATED_TEXT),
        Some(r) if r.can_end == Some(false) => {
            ("The right to end it is withheld", theme::ELEVATED_TEXT)
        }
        Some(r) if r.open_error.is_some() => {
            ("Windows does not let Anabolic end it", theme::ELEVATED_TEXT)
        }
        _ if p.ending => ("Windows is ending it but cannot finish", theme::BAD),
        _ if p.threads == 0 => ("It has no threads", theme::ELEVATED_TEXT),
        _ => ("It could not be ended", theme::ELEVATED_TEXT),
    };
    Frame::new()
        .fill(theme::RAISED)
        .stroke(Stroke::new(1.0, color))
        .inner_margin(Margin::symmetric(12, 8))
        .corner_radius(4)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(widgets::icon(theme::icon::WARNING, 14.0).color(color));
                ui.label(widgets::strong(title, theme::BODY).color(color));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if view.pending == Some(p.key) {
                        ui.spinner();
                    } else if ui
                        .small_button("Check again")
                        .on_hover_text("Reads its state and the system handle table again.")
                        .clicked()
                    {
                        out.push(UiCmd::WhyAlive(p.key));
                    }
                });
            });
            if let Some((key, e)) = &view.error
                && *key == p.key
            {
                ui.label(
                    RichText::new(format!("Its state could not be read: {e}"))
                        .color(theme::ELEVATED_TEXT),
                );
            }
            let Some(r) = report else {
                ui.label(RichText::new("Reading its state\u{2026}").color(theme::TEXT_WEAK));
                return;
            };
            let weak = |ui: &mut Ui, text: String| {
                ui.add(Label::new(RichText::new(text).color(theme::TEXT_WEAK)).wrap());
            };
            let size = fmt::bytes(p.private_bytes);
            if r.flags.deleting && r.threads == 0 {
                weak(
                    ui,
                    format!(
                        "It has no threads left and does nothing (0 % CPU, {size}). Every new attempt to end it \
                         is answered \u{201c}the process is terminating\u{201d}: there is nothing left to end. \
                         Windows removes it once the last handle to it is closed."
                    ),
                );
            } else if r.flags.deleting {
                weak(
                    ui,
                    format!(
                        "{} threads are still unwinding inside Windows. A thread that does not finish is \
                         usually waiting for a driver to complete an I/O request (graphics, anti-cheat, \
                         antivirus, a device that went away). The Threads tab shows what they wait for.",
                        r.threads
                    ),
                );
            } else if r.threads == 0 {
                weak(
                    ui,
                    format!(
                        "It never started, or all its threads are gone: it does nothing (0 % CPU, {size}). \
                         Ending it should work."
                    ),
                );
            }
            if r.protection != 0 {
                weak(
                    ui,
                    format!(
                        "It runs as a protected process ({}): not even an administrator may end it. \
                         Windows itself ends it, or the program that owns it.",
                        anabolic_core::ending::protection_label(r.protection)
                    ),
                );
            }
            if r.can_end == Some(false) && r.protection == 0 {
                let guards: Vec<String> = r
                    .guards
                    .iter()
                    .map(|(file, product)| format!("{product} ({file})"))
                    .collect();
                weak(
                    ui,
                    format!(
                        "Windows gave Anabolic a handle to it without the right to end it: a driver strips \
                         that right from every handle opened to it.{} It can be ended once that driver lets \
                         go, usually when the program it guards closes.",
                        if guards.is_empty() {
                            String::new()
                        } else {
                            format!(" Loaded drivers that do this: {}.", guards.join(", "))
                        }
                    ),
                );
            }
            if let Some(e) = &r.open_error {
                weak(
                    ui,
                    format!(
                        "Opening it to end it was refused: {e}. If Anabolic is not running as administrator, \
                         restarting it so may help."
                    ),
                );
            }
            if !r.flags.deleting && r.can_end == Some(true) && r.protection == 0 && r.threads > 0 {
                weak(
                    ui,
                    "Windows grants the right to end it and sees no protection; the refusal may have been \
                     momentary."
                        .into(),
                );
            }
            if !r.flags.deleting
                && r.can_end != Some(false)
                && r.protection == 0
                && ui.button("End it").clicked()
            {
                out.push(UiCmd::Kill {
                    key: p.key,
                    tree: false,
                });
            }

            ui.add_space(6.0);
            if r.holders.is_empty() {
                weak(
                    ui,
                    if r.flags.deleting {
                        "No program holds a handle to it, so only Windows itself does (a driver). If it stays, \
                         only a restart removes it; it does no harm meanwhile."
                            .into()
                    } else {
                        "No other program holds a handle to it.".into()
                    },
                );
                return;
            }
            ui.label(widgets::strong("Programs holding it", theme::META).color(theme::TEXT_WEAK));
            for h in &r.holders {
                ui.horizontal(|ui| {
                    process_link(ui, h.pid, p.pid, snap, out);
                    ui.label(
                        RichText::new(format!(
                            "{} {}  \u{b7}  {}",
                            h.handles.len(),
                            if h.handles.len() == 1 { "handle" } else { "handles" },
                            process_access(h.access)
                        ))
                        .color(theme::TEXT_WEAK),
                    );
                    if ui
                        .small_button("Close them\u{2026}")
                        .on_hover_text(
                            "Closes these handles inside that program, without it knowing.",
                        )
                        .clicked()
                    {
                        out.push(UiCmd::ConfirmCloseHandles {
                            pid: h.pid,
                            process: h.name.clone(),
                            handles: h.handles.clone(),
                            what: format!("{} ({})", p.name, p.pid),
                        });
                    }
                });
            }
            if r.flags.deleting {
                weak(
                    ui,
                    "While they hold it, Windows cannot remove it. It goes away when they close these \
                     handles, at the latest when they exit."
                        .into(),
                );
            }
        });
    ui.add_space(8.0);
}

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
                            &format!("{:X}", e.value),
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

fn header(
    ui: &mut Ui,
    icon: Option<egui::TextureId>,
    title: &str,
    badge: Option<String>,
    sub: &str,
    right: impl FnOnce(&mut Ui),
) {
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
        ui.with_layout(Layout::right_to_left(Align::Center), right);
    });
    ui.separator();
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
        ui.label(RichText::new("The process has exited.").color(theme::TEXT_WEAK));
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
    header(ui, icon, &p.name, Some(badge), &sub, |ui| {
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
                .on_hover_text(
                    "What it does from now on, step by step, on the Activity page: files, \
                     registry, network, programs started. In the tree view, with the processes \
                     under it.",
                )
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
                        row(ui, "Integrity", security);
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

fn waits_tab(ui: &mut Ui, p: &ProcessInfo, snap: &Snapshot, view: &WaitView, out: &mut Vec<UiCmd>) {
    if p.threads == 0 && !p.is_pseudo() {
        ui.label(
            RichText::new(
                "It has no threads: nothing in it can wait. General says why it is still here.",
            )
            .color(theme::TEXT_WEAK),
        );
        return;
    }
    let hung: Vec<_> = snap.system.hung.iter().filter(|h| h.pid == p.pid).collect();
    let report = view.report.as_deref().filter(|r| r.key == p.key);
    ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            if hung.is_empty() {
                ui.label(RichText::new("None of its windows is hung right now.").color(theme::TEXT_WEAK));
            }
            for h in &hung {
                ui.horizontal(|ui| {
                    ui.label(widgets::icon(theme::icon::WARNING, 13.0).color(theme::BAD));
                    ui.label(widgets::strong(
                        format!("\u{201c}{}\u{201d} is not responding", h.title),
                        theme::BODY,
                    ));
                    ui.label(
                        RichText::new(format!("its thread {} has taken no message for 5 s", h.tid))
                            .color(theme::TEXT_WEAK),
                    );
                });
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if view.pending == Some(p.key) {
                    ui.spinner();
                    let mut text = "Following the wait chains\u{2026}".to_string();
                    if let Some((done, total)) = view.progress {
                        text.push_str(&format!("  {done} of {total} threads"));
                    }
                    if let Some(started) = view.started {
                        text.push_str(&format!("  \u{b7}  {} s", started.elapsed().as_secs()));
                    }
                    ui.label(RichText::new(text).color(theme::TEXT_WEAK));
                    if ui
                        .small_button("Stop")
                        .on_hover_text("Stop the analysis; nothing is shown for it.")
                        .clicked()
                    {
                        out.push(UiCmd::StopWaitChain);
                    }
                    // The seconds count up.
                    ui.ctx()
                        .request_repaint_after(std::time::Duration::from_millis(500));
                } else if view.pending.is_some() {
                    ui.label(
                        RichText::new("Another process\u{2019} wait chain is being analysed.")
                            .color(theme::TEXT_WEAK),
                    );
                    if ui.small_button("Stop it").clicked() {
                        out.push(UiCmd::StopWaitChain);
                    }
                } else if ui
                    .button(if report.is_some() { "Analyze again" } else { "Analyze wait chain" })
                    .on_hover_text(
                        "Asks Windows what each of its threads is waiting for, across processes \
                         (opens its threads for reading).",
                    )
                    .clicked()
                {
                    out.push(UiCmd::ConfirmWaitChain(p.key));
                }
                if let Some(r) = report
                    && view.pending != Some(p.key)
                {
                    let mut text = format!(
                        "{} threads, {} waiting on something",
                        r.threads,
                        r.chains.iter().filter(|c| c.links.len() > 1).count(),
                    );
                    if r.idle > 0 {
                        text.push_str(&format!(
                            " \u{b7} {} running or idle, not followed",
                            r.idle
                        ));
                    }
                    if r.failed > 0 {
                        text.push_str(&format!(", {} could not be analysed", r.failed));
                    }
                    text.push_str(&format!(" \u{b7} {}", fmt::millis(r.took)));
                    ui.label(RichText::new(text).size(theme::META).color(theme::TEXT_WEAK));
                }
            });
            if let Some((key, e)) = &view.error
                && *key == p.key
            {
                ui.label(
                    RichText::new(format!("The wait chain could not be read: {e}"))
                        .color(theme::ELEVATED_TEXT),
                );
            }
            let Some(r) = report else {
                return;
            };
            if r.skipped > 0 {
                ui.label(
                    RichText::new(format!(
                        "{} threads were skipped: their chain led into something that did not answer \
                         within 2.5 s, or the 20 s limit ran out. Analyze again to retry them.",
                        r.skipped
                    ))
                    .color(theme::ELEVATED_TEXT),
                );
            }
            ui.add_space(8.0);
            if r.chains.is_empty() {
                ui.label(
                    RichText::new(
                        "No thread is waiting on anything Windows can follow: \
                         if it is slow, it is busy, not stuck.",
                    )
                    .color(theme::TEXT_WEAK),
                );
            }
            let hung_tids: Vec<u32> = hung.iter().map(|h| h.tid).collect();
            for (i, chain) in r.chains.iter().enumerate() {
                if i > 0 {
                    ui.add_space(4.0);
                    ui.separator();
                }
                chain_view(ui, chain, p.pid, &hung_tids, snap, out);
            }
        });
}

fn chain_view(
    ui: &mut Ui,
    chain: &Chain,
    own: u32,
    hung_tids: &[u32],
    snap: &Snapshot,
    out: &mut Vec<UiCmd>,
) {
    if chain.cycle {
        ui.label(
            widgets::strong("Deadlock: these threads wait on each other", theme::BODY)
                .color(theme::BAD),
        );
    }
    for (depth, link) in chain.links.iter().enumerate() {
        ui.horizontal(|ui| {
            ui.add_space(depth as f32 * 16.0);
            if depth > 0 {
                ui.label(RichText::new("\u{2192}").color(theme::TEXT_DIM));
            }
            match link {
                Link::Thread {
                    pid, tid, status, ..
                } => {
                    let label = if depth == 0 {
                        "Thread"
                    } else {
                        "held by thread"
                    };
                    ui.label(format!("{label} {tid} of"));
                    process_link(ui, *pid, own, snap, out);
                    let color = match status {
                        Status::Blocked => theme::ELEVATED_TEXT,
                        Status::Running => theme::GOOD,
                        _ => theme::TEXT_WEAK,
                    };
                    ui.label(RichText::new(status.label()).color(color));
                    if depth == 0 && hung_tids.contains(tid) {
                        ui.label(
                            RichText::new("\u{b7} the hung window's thread")
                                .size(theme::META)
                                .color(theme::BAD),
                        );
                    }
                    if depth == 0 && *status == Status::Running {
                        ui.label(
                            RichText::new("\u{b7} running, not waiting: busy, not stuck")
                                .size(theme::META)
                                .color(theme::TEXT_WEAK),
                        );
                    }
                }
                Link::Object { wait, name, status } => {
                    ui.label(format!("waits on {}", wait.label()));
                    if !name.is_empty() {
                        ui.label(RichText::new(name).monospace().color(theme::TEXT_WEAK));
                    }
                    if !matches!(status, Status::Blocked | Status::Owned | Status::Unknown) {
                        ui.label(RichText::new(status.label()).color(theme::TEXT_WEAK));
                    }
                }
            }
        });
    }
}

#[allow(clippy::too_many_arguments)]
fn network_tab(
    ui: &mut Ui,
    p: &ProcessInfo,
    snap: &Snapshot,
    tables: &mut DetailTables,
    path: Option<&str>,
    blocked: bool,
    out: &mut Vec<UiCmd>,
) {
    if blocked && let Some(path) = path {
        ui.horizontal(|ui| {
            ui.label(widgets::icon(theme::icon::LOCK, 12.0).color(theme::ELEVATED_TEXT));
            ui.label(
                RichText::new("Blocked from the network by Anabolic (Windows Firewall rules).")
                    .color(theme::ELEVATED_TEXT),
            );
            if ui.small_button("Allow again").clicked() {
                out.push(UiCmd::Unblock(path.to_string()));
            }
        });
    }
    if let Some(e) = &snap.system.net_trace_error {
        ui.label(
            RichText::new(format!("Per-process network traffic is unavailable: {e}"))
                .color(theme::ELEVATED_TEXT),
        );
        return;
    }
    let Some(sel) = snap.selected.as_ref().filter(|s| s.key == p.key) else {
        ui.label(RichText::new("Collecting…").color(theme::TEXT_WEAK));
        return;
    };
    let summary = format!(
        "Receive {}   Send {}   ·   since Anabolic started: {} received, {} sent",
        fmt::rate_always(p.net_rx_rate),
        fmt::rate_always(p.net_tx_rate),
        fmt::bytes(p.net_rx_total),
        fmt::bytes(p.net_tx_total)
    );
    if sel.connections.is_empty() {
        ui.label(RichText::new(summary).color(theme::TEXT_WEAK));
        ui.label(RichText::new("No traffic seen yet.").color(theme::TEXT_DIM));
        return;
    }
    let proto_of = |c: &anabolic_core::Connection| format!("{:?}", c.proto).to_uppercase();
    let ctx = ui.ctx().clone();
    let view = &mut tables.network;
    let names = &mut tables.names;
    let mut rows: Vec<(&anabolic_core::Connection, String, egui::Color32)> = sel
        .connections
        .iter()
        .map(|c| {
            use crate::netnames::Name;
            let (name, color) = match names.get(&ctx, c.remote) {
                Name::Dns(n) => (n.to_string(), theme::TEXT_WEAK),
                Name::Kind(k) => (k.to_string(), theme::TEXT_DIM),
                Name::Looking => ("…".to_string(), theme::TEXT_DIM),
                Name::None => (String::new(), theme::TEXT_DIM),
            };
            (c, name, color)
        })
        .filter(|(c, name, _)| {
            view.matches(&[
                &proto_of(c),
                &crate::panes::remote_text(c),
                &c.local_port.to_string(),
                name,
                crate::panes::port_service(c.proto, c.remote_port).unwrap_or(""),
            ])
        })
        .collect();
    // A fixed order first: rows the chosen column ties (same name, no
    // traffic) would otherwise trade places on every frame.
    rows.sort_by_key(|(c, ..)| (c.proto as u8, c.remote, c.remote_port, c.local_port));
    view.order(&mut rows, |(a, an, _), (b, bn, _), column| match column {
        0 => proto_of(a).cmp(&proto_of(b)),
        1 => (a.remote, a.remote_port).cmp(&(b.remote, b.remote_port)),
        2 => text_cmp(an, bn),
        3 => a.local_port.cmp(&b.local_port),
        4 => a.rx_rate.total_cmp(&b.rx_rate),
        5 => a.tx_rate.total_cmp(&b.tx_rate),
        6 => a.rx_total.cmp(&b.rx_total),
        7 => a.tx_total.cmp(&b.tx_total),
        _ => a.idle.cmp(&b.idle),
    });
    ui.horizontal(|ui| {
        if view.filter_box(
            ui,
            "Filter: address, name, port, protocol",
            rows.len(),
            sel.connections.len(),
        ) {
            ui.ctx().request_repaint();
        }
        ui.label(
            RichText::new(format!("{summary}  ·  click one for its details"))
                .small()
                .color(theme::TEXT_WEAK),
        );
    });
    let key_of = |c: &anabolic_core::Connection| (c.proto, c.remote, c.remote_port, c.local_port);
    let selected = tables.connection;
    if let Some((c, name, _)) = selected.and_then(|k| rows.iter().find(|(c, _, _)| key_of(c) == k))
    {
        let row = crate::panes::ConnRow {
            c,
            name,
            p,
            path,
            blocked,
        };
        crate::panes::side(ui, ("connection-pane", p.key), |ui| {
            crate::panes::connection_pane(ui, &row, out)
        });
    }
    let mut clicked = None;
    egui::CentralPanel::no_frame().show(ui, |ui| {
        TableBuilder::new(ui)
            .id_salt("connections")
            .striped(true)
            .resizable(true)
            .auto_shrink([false, false])
            .sense(Sense::click())
            .cell_layout(Layout::left_to_right(Align::Center))
            .column(Column::initial(50.0))
            .column(Column::initial(230.0).clip(true))
            .column(Column::initial(230.0).clip(true))
            .column(Column::initial(76.0))
            .column(Column::initial(90.0))
            .column(Column::initial(90.0))
            .column(Column::initial(90.0))
            .column(Column::initial(90.0))
            .column(Column::remainder())
            .header(20.0, |mut h| {
                for (i, (t, numeric)) in [
                    ("Proto", false),
                    ("Remote", false),
                    ("Name", false),
                    ("Local port", true),
                    ("Receive", true),
                    ("Send", true),
                    ("Received", true),
                    ("Sent", true),
                    ("Idle", true),
                ]
                .into_iter()
                .enumerate()
                {
                    h.col(|ui| {
                        view.header(ui, i, t, numeric);
                    });
                }
            })
            .body(|body| {
                body.rows(18.0, rows.len(), |mut row| {
                    let (c, name, name_color) = &rows[row.index()];
                    let c = *c;
                    let key = key_of(c);
                    row.set_selected(selected == Some(key));
                    let quiet = c.rx_rate + c.tx_rate == 0.0;
                    let color = if quiet { theme::TEXT_WEAK } else { theme::TEXT };
                    row.col(|ui| {
                        ui.label(RichText::new(proto_of(c)).color(color));
                    });
                    row.col(|ui| {
                        let response = ui.add(
                            Label::new(
                                RichText::new(crate::panes::remote_text(c))
                                    .monospace()
                                    .color(color),
                            )
                            .truncate(),
                        );
                        if let Some(service) = crate::panes::port_service(c.proto, c.remote_port) {
                            response.on_hover_text(format!(
                                "Port {}: usually {service}",
                                c.remote_port
                            ));
                        }
                    });
                    row.col(|ui| {
                        ui.add(
                            Label::new(RichText::new(name.as_str()).color(*name_color)).truncate(),
                        );
                    });
                    row.col(|ui| {
                        right(
                            ui,
                            RichText::new(c.local_port.to_string()).color(theme::TEXT_WEAK),
                        );
                    });
                    row.col(|ui| right(ui, RichText::new(fmt::rate(c.rx_rate)).color(color)));
                    row.col(|ui| right(ui, RichText::new(fmt::rate(c.tx_rate)).color(color)));
                    row.col(|ui| {
                        right(
                            ui,
                            RichText::new(fmt::bytes(c.rx_total)).color(theme::TEXT_WEAK),
                        )
                    });
                    row.col(|ui| {
                        right(
                            ui,
                            RichText::new(fmt::bytes(c.tx_total)).color(theme::TEXT_WEAK),
                        )
                    });
                    row.col(|ui| {
                        let idle = if c.idle == 0 {
                            String::new()
                        } else {
                            format!("{} s", c.idle)
                        };
                        right(ui, RichText::new(idle).color(theme::TEXT_DIM));
                    });
                    let response = row.response();
                    if response.clicked() {
                        clicked = Some(key);
                    }
                    response.context_menu(|ui| {
                        let r = crate::panes::ConnRow {
                            c,
                            name,
                            p,
                            path,
                            blocked,
                        };
                        crate::panes::connection_menu(ui, &r, out);
                    });
                });
            });
    });
    if let Some(key) = clicked {
        tables.connection = if selected == Some(key) {
            None
        } else {
            Some(key)
        };
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
    } else if p.pid == 4 {
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
        let mut table = TableBuilder::new(ui);
        if let Some((tid, scrolled)) = focus
            && !*scrolled
            && let Some(row) = list.iter().position(|r| r.t.tid == *tid)
        {
            table = table.scroll_to_row(row, Some(Align::Center));
            *scrolled = true;
        }
        table
            .id_salt("threads")
            .striped(true)
            .resizable(true)
            .auto_shrink([false, false])
            .sense(Sense::click())
            .cell_layout(Layout::left_to_right(Align::Center))
            .column(Column::initial(70.0))
            .column(Column::initial(60.0))
            .column(Column::initial(100.0))
            .column(Column::initial(150.0).clip(true))
            .column(Column::initial(280.0).clip(true))
            .column(Column::initial(70.0))
            .column(Column::initial(90.0))
            .column(Column::initial(90.0))
            .column(Column::remainder())
            .header(20.0, |mut h| {
                for (i, (t, numeric)) in [
                    ("TID", true),
                    ("CPU", true),
                    ("CPU time", true),
                    ("Name", false),
                    ("Start address", false),
                    ("Priority", true),
                    ("Ctx sw/s", true),
                    ("State", false),
                    ("Wait reason", false),
                ]
                .into_iter()
                .enumerate()
                {
                    h.col(|ui| {
                        view.header(ui, i, t, numeric);
                    });
                }
            })
            .body(|body| {
                body.rows(18.0, list.len(), |mut row| {
                    let r = &list[row.index()];
                    let t = r.t;
                    row.set_selected(focus_tid == Some(t.tid));
                    row.col(|ui| right(ui, r.tid.clone()));
                    row.col(|ui| right(ui, fmt::cpu(t.cpu)));
                    row.col(|ui| right(ui, fmt::cpu_time(t.kernel_time + t.user_time)));
                    row.col(|ui| {
                        ui.add(Label::new(r.name).truncate());
                    });
                    row.col(|ui| {
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
                    });
                    row.col(|ui| right(ui, format!("{} / {}", t.priority, t.base_priority)));
                    row.col(|ui| right(ui, fmt::count(t.context_switches_delta as u64)));
                    row.col(|ui| {
                        ui.label(thread_state_name(t.state));
                    });
                    row.col(|ui| {
                        ui.label(RichText::new(r.wait).color(theme::TEXT_WEAK));
                    });
                    let response = row.response();
                    if response.clicked() {
                        *focus = if focus_tid == Some(t.tid) {
                            None
                        } else {
                            Some((t.tid, true))
                        };
                    }
                    response.context_menu(|ui| {
                        crate::panes::thread_menu(ui, p, t, &r.start, out);
                    });
                });
            });
    });
}

fn handles_tab(
    ui: &mut Ui,
    p: &ProcessInfo,
    snap: &Snapshot,
    view: &mut HandleView,
    panes: &Panes,
    out: &mut Vec<UiCmd>,
) {
    let current = view.report.as_ref().filter(|r| r.key == p.key).cloned();
    if current.is_none() && view.pending != Some(p.key) && view.error.is_none() {
        out.push(UiCmd::InspectHandles {
            key: p.key,
            names: false,
        });
    }

    ui.horizontal(|ui| {
        let busy = view.pending == Some(p.key);
        if ui.add_enabled(!busy, egui::Button::new("Refresh")).clicked() {
            out.push(UiCmd::InspectHandles { key: p.key, names: false });
        }
        if ui
            .add_enabled(!busy, egui::Button::new("Resolve names"))
            .on_hover_text(
                "Duplicates every handle into Anabolic to ask for its name (file paths, registry keys, \
                 event names, target processes). Anti-cheat protected games may treat this as tampering.",
            )
            .clicked()
        {
            out.push(UiCmd::ConfirmResolveNames(p.key));
        }
        if busy {
            ui.spinner();
            ui.label(RichText::new("Reading the handle table…").color(theme::TEXT_WEAK));
        } else if let Some(r) = &current {
            ui.label(
                RichText::new(format!(
                    "{} handles  ·  {} system-wide  ·  {}  ·  read from the kernel handle table, the process was not opened",
                    fmt::count(r.entries.len() as u64),
                    fmt::count(r.system_total as u64),
                    fmt::millis(r.took)
                ))
                .small()
                .color(theme::TEXT_WEAK),
            );
        }
        if let Some(e) = &view.error {
            ui.label(RichText::new(e).color(theme::BAD));
        }
    });
    let Some(report) = current else { return };
    if let Some(e) = &report.name_error {
        ui.label(RichText::new(format!("Names unavailable: {e}")).color(theme::ELEVATED_TEXT));
    }

    if let Some(t) = &report.process_targets {
        let alarming = t.exited > 100;
        Frame::new()
            .fill(theme::RAISED)
            .stroke(Stroke::new(1.0, if alarming { theme::ELEVATED_TEXT } else { theme::BORDER }))
            .inner_margin(Margin::symmetric(10, 6))
            .corner_radius(4)
            .show(ui, |ui| {
                ui.label(format!(
                    "{} Process handles point at {} processes: {} running, {} exited.",
                    fmt::count(t.handles as u64),
                    fmt::count(t.distinct as u64),
                    fmt::count(t.running as u64),
                    fmt::count(t.exited as u64),
                ));
                if t.exited > 0 {
                    ui.label(
                        RichText::new(format!(
                            "The {} exited processes cannot be freed while this process holds handles to them \
                             (zombies). They are released when it closes those handles or exits.",
                            fmt::count(t.exited as u64)
                        ))
                        .color(if alarming { theme::ELEVATED_TEXT } else { theme::TEXT_WEAK }),
                    );
                }
                if let Some((pid, n)) = t.top_running.first()
                    && *n > 10
                {
                    let names: Vec<String> = t
                        .top_running
                        .iter()
                        .take(4)
                        .map(|(pid, n)| {
                            let name = snap.processes.iter().find(|q| q.pid == *pid).map_or("?", |q| &q.name);
                            format!("{name} ({pid}) ×{}", fmt::count(*n as u64))
                        })
                        .collect();
                    let _ = pid;
                    ui.label(RichText::new(format!("Held most: {}", names.join(", "))).small().color(theme::TEXT_WEAK));
                }
            });
    }
    ui.add_space(4.0);

    egui::Panel::left(egui::Id::new(("handle-types", p.key)))
        .exact_size(230.0)
        .resizable(false)
        .show(ui, |ui| {
            ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    let total = report.entries.len().max(1) as f32;
                    let mut changed = false;
                    if ui
                        .selectable_label(
                            view.type_filter.is_none(),
                            format!("All types  ({})", fmt::count(report.entries.len() as u64)),
                        )
                        .clicked()
                    {
                        view.type_filter = None;
                        changed = true;
                    }
                    for tc in &report.by_type {
                        let selected = view.type_filter == Some(tc.type_index);
                        let (rect, response) = ui
                            .allocate_exact_size(vec2(ui.available_width(), 18.0), Sense::click());
                        let fill = rect.width() * tc.count as f32 / total;
                        ui.painter().rect_filled(
                            egui::Rect::from_min_size(rect.min, vec2(fill.max(1.0), rect.height())),
                            2.0,
                            theme::heat(0.6),
                        );
                        if selected || response.hovered() {
                            ui.painter().rect_stroke(
                                rect,
                                2.0,
                                Stroke::new(1.0, theme::TEXT_WEAK),
                                egui::StrokeKind::Inside,
                            );
                        }
                        ui.painter().text(
                            rect.left_center() + vec2(4.0, 0.0),
                            egui::Align2::LEFT_CENTER,
                            &*tc.name,
                            egui::FontId::proportional(12.0),
                            theme::TEXT,
                        );
                        ui.painter().text(
                            rect.right_center() - vec2(4.0, 0.0),
                            egui::Align2::RIGHT_CENTER,
                            fmt::count(tc.count as u64),
                            egui::FontId::proportional(12.0),
                            theme::TEXT_WEAK,
                        );
                        if response.clicked() {
                            view.type_filter = if selected { None } else { Some(tc.type_index) };
                            changed = true;
                        }
                    }
                    if changed {
                        view.refilter();
                    }
                });
        });

    let selected = view.selected;
    if let Some(i) = selected.and_then(|v| report.entries.iter().position(|e| e.value == v)) {
        crate::panes::side(ui, ("handle-pane", p.key), |ui| {
            crate::panes::handle_pane(ui, p, &report, i, snap, panes, out)
        });
    }
    let mut clicked: Option<u64> = None;
    egui::CentralPanel::no_frame().show(ui, |ui| {
        let mut changed = false;
        let grouped = view.by_name && report.resolved > 0;
        let shown = if grouped {
            view.group_rows.len()
        } else {
            view.rows.len()
        };
        let total = if grouped {
            report.name_groups.len()
        } else {
            report.entries.len()
        };
        ui.horizontal(|ui| {
            changed |= view
                .table
                .filter_box(ui, "Filter: handle, type, name", shown, total);
            if report.resolved > 0 {
                ui.add_space(8.0);
                ui.selectable_value(&mut view.by_name, true, "Grouped by name");
                ui.selectable_value(&mut view.by_name, false, "Every handle");
                if report.resolved < report.entries.len() {
                    ui.label(
                        RichText::new(format!(
                            "names for the first {}",
                            fmt::count(report.resolved as u64)
                        ))
                        .small()
                        .color(theme::TEXT_WEAK),
                    );
                }
            }
        });
        if grouped {
            let groups = &view.group_rows;
            TableBuilder::new(ui)
                .id_salt("handle-names")
                .striped(true)
                .resizable(true)
                .auto_shrink([false, false])
                .sense(Sense::click())
                .cell_layout(Layout::left_to_right(Align::Center))
                .column(Column::initial(80.0))
                .column(Column::initial(120.0))
                .column(Column::remainder().clip(true))
                .header(20.0, |mut h| {
                    for (i, (t, numeric)) in
                        [("Count", true), ("Type", false), ("Name / target", false)]
                            .into_iter()
                            .enumerate()
                    {
                        h.col(|ui| {
                            changed |= view.groups.header(ui, i, t, numeric);
                        });
                    }
                })
                .body(|body| {
                    body.rows(18.0, groups.len(), |mut row| {
                        let (t, name, count) = &report.name_groups[groups[row.index()] as usize];
                        row.col(|ui| right(ui, fmt::count(*count as u64)));
                        row.col(|ui| {
                            ui.label(&*report.type_name(*t));
                        });
                        row.col(|ui| {
                            ui.add(Label::new(&**name).truncate());
                        });
                        row.response().context_menu(|ui| {
                            crate::panes::group_menu(ui, &report.type_name(*t), name, out);
                        });
                    });
                });
        } else {
            let rows = &view.rows;
            TableBuilder::new(ui)
                .id_salt("handle-list")
                .striped(true)
                .resizable(true)
                .auto_shrink([false, false])
                .sense(Sense::click())
                .cell_layout(Layout::left_to_right(Align::Center))
                .column(Column::initial(90.0))
                .column(Column::initial(120.0))
                .column(Column::initial(100.0))
                .column(Column::remainder().clip(true))
                .header(20.0, |mut h| {
                    for (i, t) in ["Handle", "Type", "Access", "Name / target"]
                        .into_iter()
                        .enumerate()
                    {
                        h.col(|ui| {
                            changed |= view.table.header(ui, i, t, false);
                        });
                    }
                })
                .body(|body| {
                    body.rows(18.0, rows.len(), |mut row| {
                        let i = rows[row.index()] as usize;
                        let e = &report.entries[i];
                        row.set_selected(selected == Some(e.value));
                        row.col(|ui| {
                            ui.label(RichText::new(format!("0x{:X}", e.value)).monospace());
                        });
                        row.col(|ui| {
                            ui.label(&*report.type_name(e.type_index));
                        });
                        row.col(|ui| {
                            ui.label(
                                RichText::new(format!("0x{:08X}", e.access))
                                    .monospace()
                                    .color(theme::TEXT_WEAK),
                            );
                        });
                        row.col(|ui| {
                            if let Some(Some(name)) = report.names.get(i) {
                                ui.add(Label::new(&**name).truncate());
                            }
                        });
                        let response = row.response();
                        if response.clicked() {
                            clicked = Some(e.value);
                        }
                        response.context_menu(|ui| {
                            let name = report.names.get(i).cloned().flatten();
                            crate::panes::handle_menu(
                                ui,
                                p,
                                &report.type_name(e.type_index),
                                e,
                                name.as_deref(),
                                out,
                            );
                        });
                    });
                });
        }
        if changed {
            view.refilter();
        }
    });
    if let Some(value) = clicked {
        view.selected = if selected == Some(value) {
            None
        } else {
            Some(value)
        };
    }
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
    header(
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
                if p.pid == 4 {
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
        TableBuilder::new(ui)
            .id_salt("modules")
            .striped(true)
            .resizable(true)
            .auto_shrink([false, false])
            .sense(Sense::click())
            .cell_layout(Layout::left_to_right(Align::Center))
            .column(Column::initial(170.0).clip(true))
            .column(Column::initial(150.0))
            .column(Column::initial(80.0))
            .column(Column::initial(220.0).clip(true))
            .column(Column::initial(150.0).clip(true))
            .column(Column::initial(110.0).clip(true))
            .column(Column::remainder().clip(true))
            .header(20.0, |mut h| {
                for (i, (t, numeric)) in [
                    ("Name", false),
                    ("Base", false),
                    ("Size", true),
                    ("Description", false),
                    ("Company", false),
                    ("Version", false),
                    ("Path", false),
                ]
                .into_iter()
                .enumerate()
                {
                    h.col(|ui| {
                        view.header(ui, i, t, numeric);
                    });
                }
            })
            .body(|body| {
                body.rows(18.0, rows.len(), |mut row| {
                    let m = rows[row.index()];
                    row.set_selected(selected == Some(m.info.base));
                    row.col(|ui| {
                        ui.add(Label::new(&*m.info.name).truncate());
                    });
                    row.col(|ui| {
                        ui.label(
                            RichText::new(format!("0x{:X}", m.info.base))
                                .monospace()
                                .color(theme::TEXT_WEAK),
                        );
                    });
                    row.col(|ui| right(ui, fmt::bytes(m.info.size)));
                    row.col(|ui| {
                        ui.add(Label::new(m.description.clone().unwrap_or_default()).truncate());
                    });
                    row.col(|ui| {
                        ui.add(
                            Label::new(
                                RichText::new(m.company.clone().unwrap_or_default())
                                    .color(theme::TEXT_WEAK),
                            )
                            .truncate(),
                        );
                    });
                    row.col(|ui| {
                        ui.add(
                            Label::new(
                                RichText::new(m.version.clone().unwrap_or_default())
                                    .color(theme::TEXT_WEAK),
                            )
                            .truncate(),
                        );
                    });
                    row.col(|ui| {
                        ui.add(
                            Label::new(RichText::new(&*m.info.path).small().color(theme::TEXT_DIM))
                                .truncate(),
                        );
                    });
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
            });
    });
}

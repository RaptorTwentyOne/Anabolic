//! Bottom panel: the selected process (identity, counters, threads, handles)
//! or the selected app group (totals and members).

use std::sync::Arc;
use std::time::Instant;

use anabolic_core::actions::Priority;
use anabolic_core::handles::{HandleReport, ModuleReport};
use anabolic_core::sysinfo::{local_time, now_filetime, thread_state_name, wait_reason_name};
use anabolic_core::waits::{Chain, Link, Status, WaitReport};
use anabolic_core::{ProcKey, ProcState, ProcessInfo, Snapshot, ThreadInfo};
use eframe::egui::{
    self, Align, Frame, Grid, Label, Layout, Margin, RichText, ScrollArea, Sense, Stroke, Ui, vec2,
};
use egui_extras::{Column, TableBuilder};

use crate::fmt;
use crate::icons::IconCache;
use crate::proc_table::UiCmd;
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

/// Modules of the selected process; also used to name thread start addresses.
#[derive(Default)]
pub struct ModuleView {
    pub report: Option<Arc<ModuleReport>>,
    pub pending: Option<ProcKey>,
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
    /// Entry indices passing `type_filter`.
    rows: Vec<u32>,
}

impl HandleView {
    pub fn set_report(&mut self, report: Arc<HandleReport>) {
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

    fn refilter(&mut self) {
        let Some(report) = &self.report else { return };
        self.rows = (0..report.entries.len() as u32)
            .filter(|&i| {
                self.type_filter
                    .is_none_or(|t| report.entries[i as usize].type_index == t)
            })
            .collect();
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
    modules: &ModuleView,
    waits: &WaitView,
    focus_thread: &mut Option<(u32, bool)>,
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
        if !p.is_pseudo() && p.state != ProcState::Exited {
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
        DetailTab::General => general(ui, p, snap, details, out),
        DetailTab::Threads => threads(ui, p, snap, modules, focus_thread, out),
        DetailTab::Waits => waits_tab(ui, p, snap, waits, out),
        DetailTab::Modules => modules_tab(ui, p, modules, out),
        DetailTab::Handles => handles_tab(ui, p, snap, handles, out),
        DetailTab::Network => network_tab(ui, p, snap),
    }
}

fn general(ui: &mut Ui, p: &ProcessInfo, snap: &Snapshot, details: &Details, out: &mut Vec<UiCmd>) {
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
                                ui.label(format!("<exited> ({})", p.ppid));
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
                    });

                counters(&mut cols[1], p);
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

fn right(ui: &mut Ui, text: String) {
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

fn network_tab(ui: &mut Ui, p: &ProcessInfo, snap: &Snapshot) {
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
    ui.label(
        RichText::new(format!(
            "Receive {}   Send {}   ·   since Anabolic started: {} received, {} sent",
            fmt::rate_always(p.net_rx_rate),
            fmt::rate_always(p.net_tx_rate),
            fmt::bytes(p.net_rx_total),
            fmt::bytes(p.net_tx_total)
        ))
        .color(theme::TEXT_WEAK),
    );
    if sel.connections.is_empty() {
        ui.label(RichText::new("No traffic seen yet.").color(theme::TEXT_DIM));
        return;
    }
    let columns = [
        ("Proto", 50.0, Align::Min),
        ("Remote", 290.0, Align::Min),
        ("Local port", 80.0, Align::Max),
        ("Receive", 100.0, Align::Max),
        ("Send", 100.0, Align::Max),
        ("Received", 100.0, Align::Max),
        ("Sent", 100.0, Align::Max),
        ("Idle", 70.0, Align::Max),
    ];
    widgets::header(ui, &columns);
    ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for (i, c) in sel.connections.iter().enumerate() {
                let quiet = c.rx_rate + c.tx_rate == 0.0;
                let color = if quiet { theme::TEXT_WEAK } else { theme::TEXT };
                widgets::row(ui, i, |ui| {
                    widgets::text(
                        ui,
                        columns[0].1,
                        Align::Min,
                        RichText::new(format!("{:?}", c.proto).to_uppercase()).color(color),
                    );
                    let remote = match c.remote {
                        std::net::IpAddr::V6(v6) => format!("[{v6}]:{}", c.remote_port),
                        v4 => format!("{v4}:{}", c.remote_port),
                    };
                    widgets::text(
                        ui,
                        columns[1].1,
                        Align::Min,
                        RichText::new(remote).monospace().color(color),
                    );
                    widgets::text(
                        ui,
                        columns[2].1,
                        Align::Max,
                        RichText::new(c.local_port.to_string()).color(theme::TEXT_WEAK),
                    );
                    widgets::text(
                        ui,
                        columns[3].1,
                        Align::Max,
                        RichText::new(fmt::rate(c.rx_rate)).color(color),
                    );
                    widgets::text(
                        ui,
                        columns[4].1,
                        Align::Max,
                        RichText::new(fmt::rate(c.tx_rate)).color(color),
                    );
                    widgets::text(
                        ui,
                        columns[5].1,
                        Align::Max,
                        RichText::new(fmt::bytes(c.rx_total)).color(theme::TEXT_WEAK),
                    );
                    widgets::text(
                        ui,
                        columns[6].1,
                        Align::Max,
                        RichText::new(fmt::bytes(c.tx_total)).color(theme::TEXT_WEAK),
                    );
                    let idle = if c.idle == 0 {
                        String::new()
                    } else {
                        format!("{} s", c.idle)
                    };
                    widgets::text(
                        ui,
                        columns[7].1,
                        Align::Max,
                        RichText::new(idle).color(theme::TEXT_DIM),
                    );
                });
            }
        });
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

/// `focus`: a thread to highlight, and whether it was scrolled to already.
fn threads(
    ui: &mut Ui,
    p: &ProcessInfo,
    snap: &Snapshot,
    modules: &ModuleView,
    focus: &mut Option<(u32, bool)>,
    out: &mut Vec<UiCmd>,
) {
    let Some(sel) = snap.selected.as_ref().filter(|s| s.key == p.key) else {
        ui.label(RichText::new("Collecting…").color(theme::TEXT_WEAK));
        return;
    };
    module_loader(ui, p, modules, "Resolve start addresses", out);
    let report = modules.for_key(p.key);
    let mut list: Vec<&ThreadInfo> = sel.threads.iter().collect();
    list.sort_by(|a, b| {
        b.cpu
            .partial_cmp(&a.cpu)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.tid.cmp(&b.tid))
    });

    let focus_tid = focus.map(|(tid, _)| tid);
    let mut table = TableBuilder::new(ui);
    if let Some((tid, scrolled)) = focus
        && !*scrolled
        && let Some(row) = list.iter().position(|t| t.tid == *tid)
    {
        table = table.scroll_to_row(row, Some(Align::Center));
        *scrolled = true;
    }
    table
        .id_salt("threads")
        .striped(true)
        .resizable(true)
        .auto_shrink([false, false])
        .cell_layout(Layout::left_to_right(Align::Center))
        .column(Column::initial(70.0))
        .column(Column::initial(60.0))
        .column(Column::initial(100.0))
        .column(Column::initial(300.0).clip(true))
        .column(Column::initial(70.0))
        .column(Column::initial(100.0))
        .column(Column::initial(90.0))
        .column(Column::remainder())
        .header(20.0, |mut h| {
            for (t, numeric) in [
                ("TID", true),
                ("CPU", true),
                ("CPU time", true),
                ("Start address", false),
                ("Priority", true),
                ("Ctx sw/s", true),
                ("State", false),
                ("Wait reason", false),
            ] {
                h.col(|ui| widgets::column_header(ui, t, numeric));
            }
        })
        .body(|body| {
            body.rows(18.0, list.len(), |mut row| {
                let t = list[row.index()];
                row.set_selected(focus_tid == Some(t.tid));
                row.col(|ui| right(ui, t.tid.to_string()));
                row.col(|ui| right(ui, fmt::cpu(t.cpu)));
                row.col(|ui| right(ui, fmt::cpu_time(t.kernel_time + t.user_time)));
                row.col(|ui| {
                    let addr = if t.win32_start_address != 0 {
                        t.win32_start_address
                    } else {
                        t.start_address
                    };
                    match symbol(report, addr) {
                        Some(name) => {
                            ui.add(Label::new(RichText::new(name).monospace()).truncate())
                                .on_hover_text(format!("0x{addr:X}"));
                        }
                        None => {
                            ui.label(
                                RichText::new(format!("0x{addr:X}"))
                                    .monospace()
                                    .color(theme::TEXT_WEAK),
                            );
                        }
                    }
                });
                row.col(|ui| right(ui, format!("{} / {}", t.priority, t.base_priority)));
                row.col(|ui| right(ui, fmt::count(t.context_switches_delta as u64)));
                row.col(|ui| {
                    ui.label(thread_state_name(t.state));
                });
                row.col(|ui| {
                    if t.state == 5 {
                        ui.label(
                            RichText::new(wait_reason_name(t.wait_reason)).color(theme::TEXT_WEAK),
                        );
                    }
                });
            });
        });
}

fn handles_tab(
    ui: &mut Ui,
    p: &ProcessInfo,
    snap: &Snapshot,
    view: &mut HandleView,
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

    egui::CentralPanel::no_frame().show(ui, |ui| {
        if report.resolved > 0 {
            ui.horizontal(|ui| {
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
            });
        }
        if view.by_name && report.resolved > 0 {
            let groups: Vec<&(u16, Arc<str>, u32)> = report
                .name_groups
                .iter()
                .filter(|g| view.type_filter.is_none_or(|t| g.0 == t))
                .collect();
            TableBuilder::new(ui)
                .id_salt("handle-names")
                .striped(true)
                .resizable(true)
                .auto_shrink([false, false])
                .cell_layout(Layout::left_to_right(Align::Center))
                .column(Column::initial(80.0))
                .column(Column::initial(120.0))
                .column(Column::remainder().clip(true))
                .header(20.0, |mut h| {
                    for (t, numeric) in [("Count", true), ("Type", false), ("Name / target", false)]
                    {
                        h.col(|ui| widgets::column_header(ui, t, numeric));
                    }
                })
                .body(|body| {
                    body.rows(18.0, groups.len(), |mut row| {
                        let (t, name, count) = groups[row.index()];
                        row.col(|ui| right(ui, fmt::count(*count as u64)));
                        row.col(|ui| {
                            ui.label(&*report.type_name(*t));
                        });
                        row.col(|ui| {
                            ui.add(Label::new(&**name).truncate());
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
                .cell_layout(Layout::left_to_right(Align::Center))
                .column(Column::initial(90.0))
                .column(Column::initial(120.0))
                .column(Column::initial(100.0))
                .column(Column::remainder().clip(true))
                .header(20.0, |mut h| {
                    for t in ["Handle", "Type", "Access", "Name / target"] {
                        h.col(|ui| widgets::column_header(ui, t, false));
                    }
                })
                .body(|body| {
                    body.rows(18.0, rows.len(), |mut row| {
                        let i = rows[row.index()] as usize;
                        let e = &report.entries[i];
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
                    });
                });
        }
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

fn modules_tab(ui: &mut Ui, p: &ProcessInfo, modules: &ModuleView, out: &mut Vec<UiCmd>) {
    module_loader(ui, p, modules, "Load modules", out);
    let Some(report) = modules.for_key(p.key) else {
        return;
    };
    if let Some(e) = &report.error {
        ui.label(
            RichText::new(format!("Could not read the module list: {e}"))
                .color(theme::ELEVATED_TEXT),
        );
    }
    ui.label(
        RichText::new(format!(
            "{} modules  ·  {}{}",
            report.modules.len(),
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
    let rows = &report.modules;
    TableBuilder::new(ui)
        .id_salt("modules")
        .striped(true)
        .resizable(true)
        .auto_shrink([false, false])
        .cell_layout(Layout::left_to_right(Align::Center))
        .column(Column::initial(170.0).clip(true))
        .column(Column::initial(150.0))
        .column(Column::initial(80.0))
        .column(Column::initial(220.0).clip(true))
        .column(Column::initial(150.0).clip(true))
        .column(Column::initial(110.0).clip(true))
        .column(Column::remainder().clip(true))
        .header(20.0, |mut h| {
            for (t, numeric) in [
                ("Name", false),
                ("Base", false),
                ("Size", true),
                ("Description", false),
                ("Company", false),
                ("Version", false),
                ("Path", false),
            ] {
                h.col(|ui| widgets::column_header(ui, t, numeric));
            }
        })
        .body(|body| {
            body.rows(18.0, rows.len(), |mut row| {
                let m = &rows[row.index()];
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
            });
        });
}

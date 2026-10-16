//! The Network page: everything on the network, machine-wide. Who receives
//! and sends how much, every connection with its state and its other end's
//! name, the ports open for others to connect to and to whom, and where the
//! traffic goes. Traffic comes from the kernel network trace, connections
//! and listening ports from Windows' socket tables; no process is opened.

use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::sync::Arc;

use anabolic_core::nettrace::Proto;
use anabolic_core::sockets::TcpState;
use anabolic_core::{Connection, Listener, NetActivity, ProcKey, ProcState, ProcessInfo, Snapshot};
use eframe::egui::{self, Align, Color32, Label, Layout, RichText, Sense, Ui};
use egui_extras::{Column, TableBuilder};

use crate::graph::Graph;
use crate::netnames::{Name, Names};
use crate::panes::{ConnRow, connection_menu, connection_pane, port_service, remote_text};
use crate::proc_table::{UiCmd, item};
use crate::table::{TableView, text_cmp};
use crate::tree::Details;
use crate::{fmt, theme, widgets};

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum NetTab {
    #[default]
    Programs,
    Connections,
    Listening,
    Destinations,
    /// The names programs looked up.
    Lookups,
}

/// A connection, by everything that tells two apart.
type ConnId = (u32, Proto, IpAddr, u16, u16);

fn id(c: &Connection) -> ConnId {
    (c.pid, c.proto, c.remote, c.remote_port, c.local_port)
}

pub struct NetworkPage {
    pub tab: NetTab,
    programs: TableView,
    connections: TableView,
    program_connections: TableView,
    listening: TableView,
    destinations: TableView,
    lookups: TableView,
    /// The program whose connections show under the list.
    pub program: Option<u32>,
    /// The connection whose side pane is open.
    connection: Option<ConnId>,
    /// Also list open connections that carried nothing while Anabolic
    /// watched.
    pub quiet: bool,
}

impl Default for NetworkPage {
    fn default() -> Self {
        Self {
            tab: NetTab::Programs,
            // Busiest first.
            programs: TableView::sorted(2, true),
            connections: TableView::sorted(6, true),
            program_connections: TableView::sorted(6, true),
            listening: TableView::sorted(4, false),
            destinations: TableView::sorted(4, true),
            // Newest first.
            lookups: TableView::sorted(0, true),
            program: None,
            connection: None,
            quiet: true,
        }
    }
}

/// The live process with this PID.
fn process(snap: &Snapshot, pid: u32) -> Option<&ProcessInfo> {
    let at = snap.processes.partition_point(|p| p.pid < pid);
    snap.processes[at..]
        .iter()
        .take_while(|p| p.pid == pid)
        .find(|p| p.state != ProcState::Exited)
}

fn process_name(snap: &Snapshot, pid: u32) -> Arc<str> {
    match pid {
        0 => "Idle".into(),
        _ => process(snap, pid).map_or_else(|| format!("PID {pid}").into(), |p| p.name.clone()),
    }
}

fn name_of(names: &mut Names, ctx: &egui::Context, ip: IpAddr) -> (String, Color32) {
    match names.get(ctx, ip) {
        Name::Dns(n) => (n.to_string(), theme::TEXT_WEAK),
        Name::Kind(k) => (k.to_string(), theme::TEXT_DIM),
        Name::Looking => ("…".to_string(), theme::TEXT_DIM),
        Name::None => (String::new(), theme::TEXT_DIM),
    }
}

fn state_text(c: &Connection) -> (&'static str, Color32) {
    match c.state {
        Some(TcpState::Established) => ("Open", theme::TEXT),
        Some(s) if s.live() => (s.label(), theme::TEXT),
        Some(TcpState::Closed) => ("Closed", theme::TEXT_DIM),
        Some(s) => (s.label(), theme::TEXT_WEAK),
        None => ("", theme::TEXT_DIM),
    }
}

/// Who can reach a listening port.
fn open_to(l: &Listener) -> (&'static str, Color32) {
    if l.local.is_unspecified() {
        ("every network", theme::ELEVATED_TEXT)
    } else if l.local.is_loopback() {
        ("this computer only", theme::TEXT_WEAK)
    } else {
        ("one network", theme::TEXT)
    }
}

fn right(ui: &mut Ui, text: impl Into<egui::WidgetText>) {
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        ui.add(Label::new(text).selectable(false));
    });
}

fn rate_cell(ui: &mut Ui, v: f64, color: Color32) {
    right(ui, RichText::new(fmt::rate(v)).color(color));
}

/// One program's share of the network.
struct Program {
    pid: u32,
    name: Arc<str>,
    key: Option<ProcKey>,
    rx: f64,
    tx: f64,
    rx_total: u64,
    tx_total: u64,
    open: usize,
    listening: usize,
    /// The other end it exchanged most with: (name or address, bytes).
    top: Option<(IpAddr, u64)>,
    /// Names it looked up lately.
    lookups: usize,
}

fn program<'a>(
    by_pid: &'a mut HashMap<u32, Program>,
    snap: &Snapshot,
    pid: u32,
) -> &'a mut Program {
    by_pid.entry(pid).or_insert_with(|| {
        let p = process(snap, pid);
        Program {
            pid,
            name: process_name(snap, pid),
            key: p.map(|p| p.key),
            rx: p.map_or(0.0, |p| p.net_rx_rate),
            tx: p.map_or(0.0, |p| p.net_tx_rate),
            rx_total: p.map_or(0, |p| p.net_rx_total),
            tx_total: p.map_or(0, |p| p.net_tx_total),
            open: 0,
            listening: 0,
            top: None,
            lookups: 0,
        }
    })
}

fn programs(snap: &Snapshot, activity: &NetActivity) -> Vec<Program> {
    let mut by_pid: HashMap<u32, Program> = HashMap::new();
    for p in snap
        .processes
        .iter()
        .filter(|p| p.state != ProcState::Exited && p.net_rx_total + p.net_tx_total > 0)
    {
        program(&mut by_pid, snap, p.pid);
    }
    for c in &activity.connections {
        let prog = program(&mut by_pid, snap, c.pid);
        if c.state.is_some_and(TcpState::live) || (c.traffic && c.idle == 0) {
            prog.open += 1;
        }
        // The machine itself, the local network and multicast chatter
        // only when there is nothing else.
        let bytes = c.rx_total + c.tx_total;
        let far = |ip: IpAddr| anabolic_core::resolver::local_kind(ip).is_none();
        let better = match prog.top {
            None => bytes > 0,
            Some((ip, b)) => match (far(c.remote), far(ip)) {
                (true, false) => bytes > 0,
                (false, true) => false,
                _ => bytes > b,
            },
        };
        if better {
            prog.top = Some((c.remote, bytes));
        }
    }
    for l in &activity.listeners {
        program(&mut by_pid, snap, l.pid).listening += 1;
    }
    for l in snap.system.dns_lookups.iter() {
        if by_pid.contains_key(&l.pid) || process(snap, l.pid).is_some() {
            program(&mut by_pid, snap, l.pid).lookups += 1;
        }
    }
    by_pid.into_values().collect()
}

fn header(ui: &mut Ui, snap: &Snapshot, activity: &NetActivity, page: &mut NetworkPage) {
    let h = &snap.history;
    let net = &snap.system.net;
    ui.horizontal(|ui| {
        let width = (ui.available_width() - 560.0).max(320.0);
        ui.vertical(|ui| {
            ui.set_width(width);
            let (response, max) = Graph::new(None)
                .series(&h.net_rx, theme::NET_RX, true)
                .series(&h.net_tx, theme::NET_TX, true)
                .show(ui, egui::vec2(width, 96.0));
            ui.painter().text(
                response.rect.left_top() + egui::vec2(6.0, 4.0),
                egui::Align2::LEFT_TOP,
                format!("{}/s", fmt::bytes(max as u64)),
                egui::FontId::proportional(theme::CAPTION),
                theme::TEXT_DIM,
            );
        });
        ui.add_space(12.0);
        let open = activity
            .connections
            .iter()
            .filter(|c| c.state.is_some_and(TcpState::live))
            .count();
        let talking = snap
            .processes
            .iter()
            .filter(|p| p.state != ProcState::Exited && p.net_total_rate() > 0.0)
            .count();
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 18.0;
                widgets::stat(
                    ui,
                    120.0,
                    "Receive",
                    RichText::new(fmt::rate_always(net.rx_rate))
                        .size(theme::SUBTITLE)
                        .color(theme::NET_RX),
                );
                widgets::stat(
                    ui,
                    120.0,
                    "Send",
                    RichText::new(fmt::rate_always(net.tx_rate))
                        .size(theme::SUBTITLE)
                        .color(theme::NET_TX),
                );
                widgets::stat(
                    ui,
                    120.0,
                    "Programs talking",
                    RichText::new(talking.to_string()).size(theme::SUBTITLE),
                );
            });
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 18.0;
                widgets::stat(
                    ui,
                    120.0,
                    "Open connections",
                    RichText::new(open.to_string()).size(theme::SUBTITLE),
                );
                widgets::stat(
                    ui,
                    120.0,
                    "Listening ports",
                    RichText::new(activity.listeners.len().to_string()).size(theme::SUBTITLE),
                );
            });
        });
    });
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        for (tab, label) in [
            (NetTab::Programs, "Programs"),
            (NetTab::Connections, "Connections"),
            (NetTab::Listening, "Listening"),
            (NetTab::Destinations, "Destinations"),
            (NetTab::Lookups, "DNS"),
        ] {
            if ui
                .add(egui::Button::selectable(page.tab == tab, label))
                .clicked()
            {
                page.tab = tab;
            }
        }
        if let Some(e) = &snap.system.net_trace_error {
            ui.label(
                RichText::new(format!("Traffic per program is unavailable: {e}"))
                    .small()
                    .color(theme::ELEVATED_TEXT),
            );
        }
        if let Some(e) = &activity.error {
            ui.label(
                RichText::new(format!("Could not read {e}"))
                    .small()
                    .color(theme::ELEVATED_TEXT),
            );
        }
    });
    ui.add_space(4.0);
}

#[allow(clippy::too_many_arguments)]
pub fn show(
    ui: &mut Ui,
    page: &mut NetworkPage,
    snap: &Snapshot,
    details: &Details,
    names: &mut Names,
    blocked: &[String],
    out: &mut Vec<UiCmd>,
) {
    let Some(activity) = snap.system.net_activity.clone() else {
        ui.add_space(20.0);
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(RichText::new("Reading the connections…").color(theme::TEXT_WEAK));
        });
        return;
    };
    header(ui, snap, &activity, page);
    let path_of = |pid: u32| {
        process(snap, pid)
            .and_then(|p| details.get(&p.key))
            .and_then(|d| d.path.clone())
    };
    match page.tab {
        NetTab::Programs => programs_tab(ui, page, snap, &activity, names, details, blocked, out),
        NetTab::Connections => {
            let rows: Vec<&Connection> = activity
                .connections
                .iter()
                .filter(|c| page.quiet || c.traffic)
                .collect();
            ui.horizontal(|ui| {
                ui.checkbox(&mut page.quiet, "Include quiet connections")
                    .on_hover_text(
                        "Open connections that carried nothing while Anabolic was watching.",
                    );
            });
            let selected = page.connection;
            let mut view = std::mem::take(&mut page.connections);
            let clicked = connections_table(
                ui,
                "all-connections",
                &rows,
                true,
                &mut view,
                selected,
                snap,
                names,
                &path_of,
                blocked,
                out,
            );
            page.connections = view;
            if let Some(c) = clicked {
                page.connection = if selected == Some(c) { None } else { Some(c) };
            }
        }
        NetTab::Listening => listening_tab(ui, page, snap, &activity, &path_of, blocked, out),
        NetTab::Destinations => destinations_tab(ui, page, snap, &activity, names),
        NetTab::Lookups => lookups_tab(ui, page, snap, out),
    }
}

#[allow(clippy::too_many_arguments)]
fn programs_tab(
    ui: &mut Ui,
    page: &mut NetworkPage,
    snap: &Snapshot,
    activity: &NetActivity,
    names: &mut Names,
    details: &Details,
    blocked: &[String],
    out: &mut Vec<UiCmd>,
) {
    let ctx = ui.ctx().clone();
    let view = &mut page.programs;
    let list = programs(snap, activity);
    let mut rows: Vec<(Program, String)> = list
        .into_iter()
        .map(|p| {
            let top = p.top.map_or(String::new(), |(ip, _)| {
                let (name, _) = name_of(names, &ctx, ip);
                if name.is_empty() || name == "…" {
                    ip.to_string()
                } else {
                    name
                }
            });
            (p, top)
        })
        .filter(|(p, top)| view.matches(&[&p.name, &p.pid.to_string(), top]))
        .collect();
    // A fixed order first: rows the chosen column ties (same name, no
    // traffic) would otherwise trade places on every frame.
    rows.sort_by(|(a, _), (b, _)| text_cmp(&a.name, &b.name).then(a.pid.cmp(&b.pid)));
    view.order(&mut rows, |(a, at), (b, bt), column| match column {
        0 => text_cmp(&a.name, &b.name),
        1 => a.pid.cmp(&b.pid),
        2 => (a.rx + a.tx).total_cmp(&(b.rx + b.tx)),
        3 => a.rx.total_cmp(&b.rx),
        4 => a.tx.total_cmp(&b.tx),
        5 => a.rx_total.cmp(&b.rx_total),
        6 => a.tx_total.cmp(&b.tx_total),
        7 => a.open.cmp(&b.open),
        8 => a.listening.cmp(&b.listening),
        9 => a.lookups.cmp(&b.lookups),
        _ => text_cmp(at, bt),
    });
    let total = rows.len();
    ui.horizontal(|ui| {
        if view.filter_box(ui, "Filter: program, PID, destination", rows.len(), total) {
            ui.ctx().request_repaint();
        }
        ui.label(
            RichText::new(
                "Totals since Anabolic started watching. Click a program for its connections.",
            )
            .small()
            .color(theme::TEXT_WEAK),
        );
    });

    // The chosen program's connections, under the list.
    if let Some(pid) = page.program {
        let mine: Vec<&Connection> = activity
            .connections
            .iter()
            .filter(|c| c.pid == pid && (page.quiet || c.traffic))
            .collect();
        let path_of = |pid: u32| {
            process(snap, pid)
                .and_then(|p| details.get(&p.key))
                .and_then(|d| d.path.clone())
        };
        let selected = page.connection;
        let mut clicked = None;
        egui::Panel::bottom("net-program-connections")
            .resizable(true)
            .default_size(ui.available_height() * 0.5)
            .min_size(120.0)
            .show(ui, |ui| {
                ui.set_min_height(ui.available_height());
                ui.horizontal(|ui| {
                    ui.label(widgets::strong(
                        format!(
                            "{} ({pid}): {} connections",
                            process_name(snap, pid),
                            mine.len()
                        ),
                        theme::BODY,
                    ));
                    if ui
                        .small_button(widgets::icon(theme::icon::CLOSE, 10.0))
                        .on_hover_text("Close")
                        .clicked()
                    {
                        page.program = None;
                    }
                });
                clicked = connections_table(
                    ui,
                    "program-connections",
                    &mine,
                    false,
                    &mut page.program_connections,
                    selected,
                    snap,
                    names,
                    &path_of,
                    blocked,
                    out,
                );
            });
        if let Some(c) = clicked {
            page.connection = if selected == Some(c) { None } else { Some(c) };
        }
    }

    let chosen = page.program;
    let mut pick = None;
    let view = &mut page.programs;
    egui::CentralPanel::no_frame().show(ui, |ui| {
        TableBuilder::new(ui)
            .id_salt("net-programs")
            .striped(true)
            .resizable(true)
            .auto_shrink([false, false])
            .sense(Sense::click())
            .cell_layout(Layout::left_to_right(Align::Center))
            .column(Column::initial(200.0).clip(true))
            .column(Column::initial(64.0))
            .column(Column::initial(90.0))
            .column(Column::initial(90.0))
            .column(Column::initial(90.0))
            .column(Column::initial(90.0))
            .column(Column::initial(90.0))
            .column(Column::initial(96.0))
            .column(Column::initial(80.0))
            .column(Column::initial(76.0))
            .column(Column::remainder().clip(true))
            .header(20.0, |mut h| {
                for (i, (t, numeric)) in [
                    ("Program", false),
                    ("PID", true),
                    ("Total", true),
                    ("Receive", true),
                    ("Send", true),
                    ("Received", true),
                    ("Sent", true),
                    ("Connections", true),
                    ("Listening", true),
                    ("Lookups", true),
                    ("Talks most to", false),
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
                    let (p, top) = &rows[row.index()];
                    row.set_selected(chosen == Some(p.pid));
                    let quiet = p.rx + p.tx == 0.0;
                    let color = if quiet { theme::TEXT_WEAK } else { theme::TEXT };
                    row.col(|ui| {
                        ui.add(Label::new(RichText::new(&*p.name).color(color)).truncate());
                    });
                    row.col(|ui| {
                        right(ui, RichText::new(p.pid.to_string()).color(theme::TEXT_WEAK))
                    });
                    row.col(|ui| rate_cell(ui, p.rx + p.tx, color));
                    row.col(|ui| rate_cell(ui, p.rx, theme::NET_RX));
                    row.col(|ui| rate_cell(ui, p.tx, theme::NET_TX));
                    row.col(|ui| {
                        right(
                            ui,
                            RichText::new(fmt::bytes(p.rx_total)).color(theme::TEXT_WEAK),
                        )
                    });
                    row.col(|ui| {
                        right(
                            ui,
                            RichText::new(fmt::bytes(p.tx_total)).color(theme::TEXT_WEAK),
                        )
                    });
                    row.col(|ui| {
                        if p.open > 0 {
                            right(ui, p.open.to_string());
                        }
                    });
                    row.col(|ui| {
                        if p.listening > 0 {
                            right(
                                ui,
                                RichText::new(p.listening.to_string()).color(theme::TEXT_WEAK),
                            );
                        }
                    });
                    row.col(|ui| {
                        if p.lookups > 0 {
                            right(
                                ui,
                                RichText::new(p.lookups.to_string()).color(theme::TEXT_WEAK),
                            );
                        }
                    });
                    row.col(|ui| {
                        ui.add(Label::new(RichText::new(top).color(theme::TEXT_WEAK)).truncate());
                    });
                    let response = row.response();
                    if response.clicked() {
                        pick = Some(p.pid);
                    }
                    response.context_menu(|ui| {
                        if let Some(key) = p.key {
                            item(ui, out, "Go to process", "", UiCmd::GoTo(key));
                        }
                        let path = p
                            .key
                            .and_then(|k| details.get(&k))
                            .and_then(|d| d.path.clone());
                        if let Some(path) = path {
                            block_item(ui, &p.name, &path, blocked, out);
                        }
                        item(ui, out, "Copy name", "", UiCmd::Copy(p.name.to_string()));
                    });
                });
            });
    });
    if let Some(pid) = pick {
        page.program = if chosen == Some(pid) { None } else { Some(pid) };
        page.connection = None;
    }
}

fn block_item(ui: &mut Ui, name: &Arc<str>, path: &str, blocked: &[String], out: &mut Vec<UiCmd>) {
    if blocked.iter().any(|b| b.eq_ignore_ascii_case(path)) {
        item(
            ui,
            out,
            "Allow network access again",
            "",
            UiCmd::Unblock(path.to_string()),
        );
    } else if !crate::proc_table::system_file(path) {
        item(
            ui,
            out,
            "Block network access\u{2026}",
            "",
            UiCmd::ConfirmBlock {
                path: path.to_string(),
                name: name.clone(),
            },
        );
    }
}

/// A table of connections with a side pane for the selected one; returns
/// the connection clicked.
#[allow(clippy::too_many_arguments)]
fn connections_table(
    ui: &mut Ui,
    salt: &str,
    list: &[&Connection],
    with_program: bool,
    view: &mut TableView,
    selected: Option<ConnId>,
    snap: &Snapshot,
    names: &mut Names,
    path_of: &dyn Fn(u32) -> Option<String>,
    blocked: &[String],
    out: &mut Vec<UiCmd>,
) -> Option<ConnId> {
    let ctx = ui.ctx().clone();
    let proto_of = |c: &Connection| format!("{:?}", c.proto).to_uppercase();
    let mut rows: Vec<(&Connection, Arc<str>, String, Color32)> = list
        .iter()
        .map(|c| {
            let (name, color) = name_of(names, &ctx, c.remote);
            (*c, process_name(snap, c.pid), name, color)
        })
        .filter(|(c, program, name, _)| {
            view.matches(&[
                program,
                &c.pid.to_string(),
                &proto_of(c),
                &remote_text(c),
                name,
                state_text(c).0,
                port_service(c.proto, c.remote_port).unwrap_or(""),
            ])
        })
        .collect();
    // A fixed order first: rows the chosen column ties (same name, no
    // traffic) would otherwise trade places on every frame.
    rows.sort_by_key(|(c, ..)| (c.pid, c.proto as u8, c.remote, c.remote_port, c.local_port));
    view.order(
        &mut rows,
        |(a, ap, an, _), (b, bp, bn, _), column| match column {
            0 => text_cmp(ap, bp),
            1 => proto_of(a).cmp(&proto_of(b)),
            2 => state_text(a).0.cmp(state_text(b).0),
            3 => (a.remote, a.remote_port).cmp(&(b.remote, b.remote_port)),
            4 => text_cmp(an, bn),
            5 => a.local_port.cmp(&b.local_port),
            6 => a.rx_rate.total_cmp(&b.rx_rate),
            7 => a.tx_rate.total_cmp(&b.tx_rate),
            8 => a.rx_total.cmp(&b.rx_total),
            9 => a.tx_total.cmp(&b.tx_total),
            _ => a.idle.cmp(&b.idle),
        },
    );
    ui.horizontal(|ui| {
        if view.filter_box(
            ui,
            "Filter: program, address, name, port, state",
            rows.len(),
            list.len(),
        ) {
            ui.ctx().request_repaint();
        }
    });
    if let Some((c, _, name, _)) = selected.and_then(|k| rows.iter().find(|(c, ..)| id(c) == k)) {
        let blank;
        let p = match process(snap, c.pid) {
            Some(p) => p,
            None => {
                blank = ProcessInfo::blank(
                    ProcKey {
                        pid: c.pid,
                        create_time: 0,
                    },
                    0,
                    process_name(snap, c.pid),
                );
                &blank
            }
        };
        let path = path_of(c.pid);
        let row = ConnRow {
            c,
            name,
            p,
            path: path.as_deref(),
            blocked: path
                .as_deref()
                .is_some_and(|p| blocked.iter().any(|b| b.eq_ignore_ascii_case(p))),
        };
        crate::panes::side(ui, ("net-pane", salt), |ui| connection_pane(ui, &row, out));
    }
    let mut clicked = None;
    egui::CentralPanel::no_frame().show(ui, |ui| {
        let mut table = TableBuilder::new(ui)
            .id_salt(salt)
            .striped(true)
            .resizable(true)
            .auto_shrink([false, false])
            .sense(Sense::click())
            .cell_layout(Layout::left_to_right(Align::Center));
        table = table.column(if with_program {
            Column::initial(170.0).clip(true)
        } else {
            Column::exact(0.0)
        });
        table = table
            .column(Column::initial(46.0))
            .column(Column::initial(110.0).clip(true))
            .column(Column::initial(220.0).clip(true))
            .column(Column::initial(220.0).clip(true))
            .column(Column::initial(72.0))
            .column(Column::initial(84.0))
            .column(Column::initial(84.0))
            .column(Column::initial(84.0))
            .column(Column::initial(84.0))
            .column(Column::remainder());
        table
            .header(20.0, |mut h| {
                for (i, (t, numeric)) in [
                    (if with_program { "Program" } else { "" }, false),
                    ("Proto", false),
                    ("State", false),
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
                        if !t.is_empty() {
                            view.header(ui, i, t, numeric);
                        }
                    });
                }
            })
            .body(|body| {
                body.rows(18.0, rows.len(), |mut row| {
                    let (c, program, name, name_color) = &rows[row.index()];
                    let c = *c;
                    row.set_selected(selected == Some(id(c)));
                    let quiet = c.rx_rate + c.tx_rate == 0.0;
                    let color = if quiet { theme::TEXT_WEAK } else { theme::TEXT };
                    row.col(|ui| {
                        if with_program {
                            ui.add(Label::new(RichText::new(&**program).color(color)).truncate());
                        }
                    });
                    row.col(|ui| {
                        ui.label(
                            RichText::new(format!("{:?}", c.proto).to_uppercase())
                                .color(theme::TEXT_WEAK),
                        );
                    });
                    row.col(|ui| {
                        let (text, color) = state_text(c);
                        ui.add(Label::new(RichText::new(text).color(color)).truncate());
                    });
                    row.col(|ui| {
                        let response = ui.add(
                            Label::new(RichText::new(remote_text(c)).monospace().color(color))
                                .truncate(),
                        );
                        if let Some(service) = port_service(c.proto, c.remote_port) {
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
                        )
                    });
                    row.col(|ui| rate_cell(ui, c.rx_rate, theme::NET_RX));
                    row.col(|ui| rate_cell(ui, c.tx_rate, theme::NET_TX));
                    row.col(|ui| {
                        if c.traffic {
                            right(
                                ui,
                                RichText::new(fmt::bytes(c.rx_total)).color(theme::TEXT_WEAK),
                            );
                        }
                    });
                    row.col(|ui| {
                        if c.traffic {
                            right(
                                ui,
                                RichText::new(fmt::bytes(c.tx_total)).color(theme::TEXT_WEAK),
                            );
                        }
                    });
                    row.col(|ui| {
                        let idle = if !c.traffic {
                            "quiet".to_string()
                        } else if c.idle == 0 {
                            String::new()
                        } else {
                            format!("{} s", c.idle)
                        };
                        right(ui, RichText::new(idle).color(theme::TEXT_DIM));
                    });
                    let response = row.response();
                    if response.clicked() {
                        clicked = Some(id(c));
                    }
                    response.context_menu(|ui| {
                        let path = path_of(c.pid);
                        if let Some(p) = process(snap, c.pid) {
                            item(ui, out, "Go to process", "", UiCmd::GoTo(p.key));
                            ui.separator();
                            let r = ConnRow {
                                c,
                                name,
                                p,
                                path: path.as_deref(),
                                blocked: path.as_deref().is_some_and(|p| {
                                    blocked.iter().any(|b| b.eq_ignore_ascii_case(p))
                                }),
                            };
                            connection_menu(ui, &r, out);
                        }
                    });
                });
            });
    });
    clicked
}

fn listening_tab(
    ui: &mut Ui,
    page: &mut NetworkPage,
    snap: &Snapshot,
    activity: &NetActivity,
    path_of: &dyn Fn(u32) -> Option<String>,
    blocked: &[String],
    out: &mut Vec<UiCmd>,
) {
    let view = &mut page.listening;
    let proto_of = |l: &Listener| format!("{:?}", l.proto).to_uppercase();
    let mut rows: Vec<(&Listener, Arc<str>)> = activity
        .listeners
        .iter()
        .map(|l| (l, process_name(snap, l.pid)))
        .filter(|(l, name)| {
            view.matches(&[
                name,
                &l.pid.to_string(),
                &proto_of(l),
                &l.local.to_string(),
                &l.port.to_string(),
                port_service(l.proto, l.port).unwrap_or(""),
                open_to(l).0,
            ])
        })
        .collect();
    // A fixed order first: rows the chosen column ties (same name, no
    // traffic) would otherwise trade places on every frame.
    rows.sort_by_key(|(l, _)| (l.port, l.proto as u8, l.local, l.pid));
    view.order(&mut rows, |(a, an), (b, bn), column| match column {
        0 => text_cmp(an, bn),
        1 => a.pid.cmp(&b.pid),
        2 => proto_of(a).cmp(&proto_of(b)),
        3 => a.local.cmp(&b.local),
        4 => a.port.cmp(&b.port).then(proto_of(a).cmp(&proto_of(b))),
        5 => port_service(a.proto, a.port)
            .unwrap_or("~")
            .cmp(port_service(b.proto, b.port).unwrap_or("~")),
        _ => open_to(a).0.cmp(open_to(b).0),
    });
    let tcp = activity
        .listeners
        .iter()
        .filter(|l| l.proto == Proto::Tcp)
        .count();
    ui.horizontal(|ui| {
        if view.filter_box(
            ui,
            "Filter: program, port, address, service",
            rows.len(),
            activity.listeners.len(),
        ) {
            ui.ctx().request_repaint();
        }
        ui.label(
            RichText::new(format!(
                "{tcp} TCP ports waiting for connections, {} UDP ports receiving  ·  \
                 \"every network\" can be reached from outside this computer unless the \
                 firewall stops it",
                activity.listeners.len() - tcp
            ))
            .small()
            .color(theme::TEXT_WEAK),
        );
    });
    egui::CentralPanel::no_frame().show(ui, |ui| {
        TableBuilder::new(ui)
            .id_salt("net-listening")
            .striped(true)
            .resizable(true)
            .auto_shrink([false, false])
            .sense(Sense::click())
            .cell_layout(Layout::left_to_right(Align::Center))
            .column(Column::initial(200.0).clip(true))
            .column(Column::initial(64.0))
            .column(Column::initial(50.0))
            .column(Column::initial(240.0).clip(true))
            .column(Column::initial(70.0))
            .column(Column::initial(220.0).clip(true))
            .column(Column::remainder())
            .header(20.0, |mut h| {
                for (i, (t, numeric)) in [
                    ("Program", false),
                    ("PID", true),
                    ("Proto", false),
                    ("Address", false),
                    ("Port", true),
                    ("Usually", false),
                    ("Open to", false),
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
                    let (l, name) = &rows[row.index()];
                    row.col(|ui| {
                        ui.add(Label::new(&**name).truncate());
                    });
                    row.col(|ui| {
                        right(ui, RichText::new(l.pid.to_string()).color(theme::TEXT_WEAK))
                    });
                    row.col(|ui| {
                        ui.label(RichText::new(proto_of(l)).color(theme::TEXT_WEAK));
                    });
                    row.col(|ui| {
                        ui.add(
                            Label::new(
                                RichText::new(l.local.to_string())
                                    .monospace()
                                    .color(theme::TEXT_WEAK),
                            )
                            .truncate(),
                        );
                    });
                    row.col(|ui| right(ui, l.port.to_string()));
                    row.col(|ui| {
                        ui.add(
                            Label::new(
                                RichText::new(port_service(l.proto, l.port).unwrap_or(""))
                                    .color(theme::TEXT_WEAK),
                            )
                            .truncate(),
                        );
                    });
                    row.col(|ui| {
                        let (text, color) = open_to(l);
                        ui.label(RichText::new(text).color(color));
                    });
                    row.response().context_menu(|ui| {
                        if let Some(p) = process(snap, l.pid) {
                            item(ui, out, "Go to process", "", UiCmd::GoTo(p.key));
                            if let Some(path) = path_of(l.pid) {
                                block_item(ui, &p.name, &path, blocked, out);
                            }
                            ui.separator();
                        }
                        let address = match l.local {
                            IpAddr::V6(v6) => format!("[{v6}]:{}", l.port),
                            v4 => format!("{v4}:{}", l.port),
                        };
                        item(ui, out, "Copy address", "", UiCmd::Copy(address));
                    });
                });
            });
    });
}

/// Traffic and connections per other end, by name when it has one.
struct Destination {
    host: String,
    addresses: HashSet<IpAddr>,
    programs: Vec<Arc<str>>,
    connections: usize,
    rx: f64,
    tx: f64,
    rx_total: u64,
    tx_total: u64,
}

fn destinations_tab(
    ui: &mut Ui,
    page: &mut NetworkPage,
    snap: &Snapshot,
    activity: &NetActivity,
    names: &mut Names,
) {
    let ctx = ui.ctx().clone();
    let mut by_host: HashMap<String, Destination> = HashMap::new();
    for c in activity
        .connections
        .iter()
        .filter(|c| c.traffic || c.state.is_some_and(TcpState::live))
    {
        let (name, _) = name_of(names, &ctx, c.remote);
        let host = if name.is_empty() || name == "…" {
            c.remote.to_string()
        } else if anabolic_core::resolver::local_kind(c.remote).is_some() {
            format!("{} ({name})", c.remote)
        } else {
            name
        };
        let d = by_host.entry(host.clone()).or_insert_with(|| Destination {
            host,
            addresses: HashSet::new(),
            programs: Vec::new(),
            connections: 0,
            rx: 0.0,
            tx: 0.0,
            rx_total: 0,
            tx_total: 0,
        });
        d.addresses.insert(c.remote);
        let program = process_name(snap, c.pid);
        if !d.programs.contains(&program) {
            d.programs.push(program);
        }
        d.connections += 1;
        d.rx += c.rx_rate;
        d.tx += c.tx_rate;
        d.rx_total += c.rx_total;
        d.tx_total += c.tx_total;
    }
    let view = &mut page.destinations;
    let mut rows: Vec<Destination> = by_host
        .into_values()
        .filter(|d| {
            let programs = d
                .programs
                .iter()
                .map(|p| &**p)
                .collect::<Vec<_>>()
                .join(" ");
            view.matches(&[&d.host, &programs])
        })
        .collect();
    let total = rows.len();
    // A fixed order first: rows the chosen column ties (same name, no
    // traffic) would otherwise trade places on every frame.
    rows.sort_by(|a, b| a.host.cmp(&b.host));
    view.order(&mut rows, |a, b, column| match column {
        0 => text_cmp(&a.host, &b.host),
        1 => a.programs.len().cmp(&b.programs.len()),
        2 => a.connections.cmp(&b.connections),
        3 => a.rx.total_cmp(&b.rx),
        4 => (a.rx_total + a.tx_total).cmp(&(b.rx_total + b.tx_total)),
        5 => a.tx.total_cmp(&b.tx),
        6 => a.rx_total.cmp(&b.rx_total),
        _ => a.tx_total.cmp(&b.tx_total),
    });
    ui.horizontal(|ui| {
        if view.filter_box(ui, "Filter: name, address, program", rows.len(), total) {
            ui.ctx().request_repaint();
        }
        ui.label(
            RichText::new(
                "Where the traffic goes, by the other end's name. Totals cover the connections \
                 seen lately; click one to see them.",
            )
            .small()
            .color(theme::TEXT_WEAK),
        );
    });
    let mut open = None;
    egui::CentralPanel::no_frame().show(ui, |ui| {
        TableBuilder::new(ui)
            .id_salt("net-destinations")
            .striped(true)
            .resizable(true)
            .auto_shrink([false, false])
            .sense(Sense::click())
            .cell_layout(Layout::left_to_right(Align::Center))
            .column(Column::initial(300.0).clip(true))
            .column(Column::initial(240.0).clip(true))
            .column(Column::initial(90.0))
            .column(Column::initial(90.0))
            .column(Column::initial(90.0))
            .column(Column::initial(90.0))
            .column(Column::initial(90.0))
            .column(Column::remainder())
            .header(20.0, |mut h| {
                for (i, (t, numeric)) in [
                    ("Destination", false),
                    ("Programs", false),
                    ("Connections", true),
                    ("Receive", true),
                    ("Total", true),
                    ("Send", true),
                    ("Received", true),
                    ("Sent", true),
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
                    let d = &rows[row.index()];
                    let quiet = d.rx + d.tx == 0.0;
                    let color = if quiet { theme::TEXT_WEAK } else { theme::TEXT };
                    row.col(|ui| {
                        let response =
                            ui.add(Label::new(RichText::new(&d.host).color(color)).truncate());
                        if d.addresses.len() > 1 {
                            let mut list: Vec<String> =
                                d.addresses.iter().map(|a| a.to_string()).collect();
                            list.sort();
                            response.on_hover_text(list.join("\n"));
                        }
                    });
                    row.col(|ui| {
                        let programs = d
                            .programs
                            .iter()
                            .map(|p| &**p)
                            .collect::<Vec<_>>()
                            .join(", ");
                        ui.add(
                            Label::new(RichText::new(programs).color(theme::TEXT_WEAK)).truncate(),
                        );
                    });
                    row.col(|ui| right(ui, d.connections.to_string()));
                    row.col(|ui| rate_cell(ui, d.rx, theme::NET_RX));
                    row.col(|ui| {
                        right(
                            ui,
                            RichText::new(fmt::bytes(d.rx_total + d.tx_total))
                                .color(theme::TEXT_WEAK),
                        )
                    });
                    row.col(|ui| rate_cell(ui, d.tx, theme::NET_TX));
                    row.col(|ui| {
                        right(
                            ui,
                            RichText::new(fmt::bytes(d.rx_total)).color(theme::TEXT_WEAK),
                        )
                    });
                    row.col(|ui| {
                        right(
                            ui,
                            RichText::new(fmt::bytes(d.tx_total)).color(theme::TEXT_WEAK),
                        )
                    });
                    if row.response().clicked() {
                        open = Some(d.host.clone());
                    }
                });
            });
    });
    // A destination opens the connections to it.
    if let Some(host) = open {
        let filter = host.split(' ').next().unwrap_or(&host).to_string();
        page.connections.set_filter(&filter);
        page.connection = None;
        page.tab = NetTab::Connections;
    }
}

fn lookups_tab(ui: &mut Ui, page: &mut NetworkPage, snap: &Snapshot, out: &mut Vec<UiCmd>) {
    use anabolic_core::nettrace::DnsLookup;
    let log = snap.system.dns_lookups.clone();
    let view = &mut page.lookups;
    let answer = |l: &DnsLookup| {
        let mut parts: Vec<String> = l.addresses.iter().map(|a| a.to_string()).collect();
        if parts.is_empty() && !l.aliases.is_empty() {
            parts.push(format!("alias of {}", l.aliases.join(" → ")));
        }
        parts.join(", ")
    };
    let mut rows: Vec<(usize, &DnsLookup, Arc<str>)> = log
        .iter()
        .enumerate()
        .map(|(i, l)| (i, l, process_name(snap, l.pid)))
        .filter(|(_, l, program)| {
            view.matches(&[
                program,
                &l.pid.to_string(),
                &l.name,
                &answer(l),
                &l.aliases.join(" "),
                &l.status_label(),
            ])
        })
        .collect();
    view.order(&mut rows, |(ai, a, ap), (bi, b, bp), column| match column {
        0 => ai.cmp(bi),
        1 => text_cmp(ap, bp),
        2 => text_cmp(&a.name, &b.name),
        3 => a.kind.cmp(&b.kind),
        4 => a.status_label().cmp(&b.status_label()),
        _ => answer(a).cmp(&answer(b)),
    });
    let failed = log.iter().filter(|l| l.status != 0).count();
    ui.horizontal(|ui| {
        if view.filter_box(
            ui,
            "Filter: program, name, address, result",
            rows.len(),
            log.len(),
        ) {
            ui.ctx().request_repaint();
        }
        ui.label(
            RichText::new(format!(
                "The names programs looked up since Anabolic started (the last {}), {failed} \
                 unanswered. The DNS client reports them from inside each program; nothing is \
                 opened.",
                log.len()
            ))
            .small()
            .color(theme::TEXT_WEAK),
        );
    });
    egui::CentralPanel::no_frame().show(ui, |ui| {
        TableBuilder::new(ui)
            .id_salt("net-lookups")
            .striped(true)
            .resizable(true)
            .auto_shrink([false, false])
            .sense(Sense::click())
            .cell_layout(Layout::left_to_right(Align::Center))
            .column(Column::initial(76.0))
            .column(Column::initial(180.0).clip(true))
            .column(Column::initial(320.0).clip(true))
            .column(Column::initial(60.0))
            .column(Column::initial(110.0))
            .column(Column::remainder().clip(true))
            .header(20.0, |mut h| {
                for (i, (t, numeric)) in [
                    ("Time", false),
                    ("Program", false),
                    ("Name", false),
                    ("Type", false),
                    ("Result", false),
                    ("Answer", false),
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
                    let (_, l, program) = &rows[row.index()];
                    let failed = l.status != 0;
                    row.col(|ui| {
                        ui.label(RichText::new(fmt::clock(l.at, false)).color(theme::TEXT_WEAK));
                    });
                    row.col(|ui| {
                        ui.add(Label::new(&**program).truncate());
                    });
                    row.col(|ui| {
                        let response = ui.add(Label::new(&*l.name).truncate());
                        if !l.aliases.is_empty() {
                            response.on_hover_text(format!(
                                "{} is an alias of {}",
                                l.name,
                                l.aliases.join(" → ")
                            ));
                        }
                    });
                    row.col(|ui| {
                        ui.label(RichText::new(l.kind_label()).color(theme::TEXT_WEAK));
                    });
                    row.col(|ui| {
                        let color = if failed {
                            theme::ELEVATED_TEXT
                        } else {
                            theme::TEXT_WEAK
                        };
                        ui.label(RichText::new(l.status_label()).color(color));
                    });
                    row.col(|ui| {
                        ui.add(
                            Label::new(
                                RichText::new(answer(l)).monospace().color(theme::TEXT_WEAK),
                            )
                            .truncate(),
                        );
                    });
                    row.response().context_menu(|ui| {
                        if let Some(p) = process(snap, l.pid) {
                            item(ui, out, "Go to process", "", UiCmd::GoTo(p.key));
                            ui.separator();
                        }
                        item(ui, out, "Copy name", "", UiCmd::Copy(l.name.to_string()));
                        item(
                            ui,
                            out,
                            "Search the name online",
                            "",
                            UiCmd::SearchOnline(l.name.to_string()),
                        );
                    });
                });
            });
    });
}

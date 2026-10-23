// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! The Network tab: a process' connections, sortable, with a side pane and
//! what can be done about each.

use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) fn network_tab(
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
    if cfg!(target_os = "linux") {
        ui.label(
            RichText::new("TCP traffic only: Linux counts no UDP traffic per program.")
                .size(theme::META)
                .color(theme::TEXT_WEAK),
        );
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

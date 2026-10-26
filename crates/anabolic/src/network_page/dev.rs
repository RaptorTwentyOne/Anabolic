// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Development ports: the dev servers, databases and debuggers listening
//! (see `devports`), and the ports you watch, free or with who holds them.

use crate::settings::PortWatch;

use super::*;

/// One port of one program, its addresses together.
pub(crate) struct DevPort {
    pub port: u16,
    /// 0 for a watched port that nothing holds.
    pub pid: u32,
    pub program: Arc<str>,
    /// Your name for it, else the tool's.
    pub tool: String,
    pub addresses: Vec<IpAddr>,
    pub open_to: (&'static str, Color32),
    pub watched: bool,
}

/// The development ports and the watched ones, one row per port and
/// program.
pub(crate) fn dev_ports(
    snap: &Snapshot,
    activity: &NetActivity,
    watched: &[PortWatch],
) -> Vec<DevPort> {
    let mut rows: Vec<DevPort> = Vec::new();
    for l in activity.listeners.iter().filter(|l| l.pid != 0) {
        let program = process_name(snap, l.pid);
        let mine = watched.iter().find(|w| w.port == l.port);
        let tool = match (mine, crate::devports::tool(l.port, &program)) {
            (Some(w), _) if !w.name.is_empty() => w.name.clone(),
            (_, Some(tool)) => tool.to_string(),
            (Some(_), None) => "watched".to_string(),
            (None, None) => continue,
        };
        let reach = open_to(l);
        match rows.iter_mut().find(|r| r.port == l.port && r.pid == l.pid) {
            Some(row) => {
                if !row.addresses.contains(&l.local) {
                    row.addresses.push(l.local);
                }
                // The widest reach of its addresses.
                if l.local.is_unspecified() {
                    row.open_to = reach;
                }
            }
            None => rows.push(DevPort {
                port: l.port,
                pid: l.pid,
                program,
                tool,
                addresses: vec![l.local],
                open_to: reach,
                watched: mine.is_some(),
            }),
        }
    }
    for w in watched {
        if !rows.iter().any(|r| r.port == w.port) {
            rows.push(DevPort {
                port: w.port,
                pid: 0,
                program: "".into(),
                tool: if w.name.is_empty() {
                    "watched".into()
                } else {
                    w.name.clone()
                },
                addresses: Vec::new(),
                open_to: ("free", theme::GOOD),
                watched: true,
            });
        }
    }
    rows
}

/// The watched ports, with what holds each and a field to add one.
fn watched_strip(
    ui: &mut Ui,
    page: &mut NetworkPage,
    rows: &[DevPort],
    watched: &[PortWatch],
    out: &mut Vec<UiCmd>,
) {
    ui.horizontal_wrapped(|ui| {
        ui.label(
            RichText::new("Watched")
                .size(theme::META)
                .color(theme::TEXT_WEAK),
        )
        .on_hover_text(
            "Ports you need, say for your own dev server: free, or who holds each. Settings has \
             them too.",
        );
        for w in watched {
            let holder = rows.iter().find(|r| r.port == w.port && r.pid != 0);
            egui::Frame::new()
                .fill(theme::RAISED)
                .stroke(egui::Stroke::new(1.0, theme::BORDER))
                .corner_radius(theme::RADIUS_CONTROL)
                .inner_margin(egui::Margin::symmetric(6, 2))
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    ui.label(widgets::strong(format!(":{}", w.port), theme::BODY));
                    if !w.name.is_empty() {
                        ui.label(RichText::new(&w.name).color(theme::TEXT_WEAK));
                    }
                    match holder {
                        Some(r) => {
                            ui.label(
                                RichText::new(format!("{} ({})", r.program, r.pid))
                                    .color(theme::TEXT_WEAK),
                            );
                            if ui
                                .small_button("Free\u{2026}")
                                .on_hover_text(format!("End {} so the port is free", r.program))
                                .clicked()
                            {
                                out.push(UiCmd::FreePort {
                                    port: w.port,
                                    pid: r.pid,
                                });
                            }
                        }
                        None => {
                            ui.label(RichText::new("free").color(theme::GOOD));
                        }
                    }
                    if ui
                        .small_button(widgets::icon(theme::icon::CLOSE, 9.0))
                        .on_hover_text("Stop watching it")
                        .clicked()
                    {
                        out.push(UiCmd::UnwatchPort(w.port));
                    }
                });
        }
        let port = ui.add(
            egui::TextEdit::singleline(&mut page.add_port)
                .hint_text("port")
                .desired_width(56.0),
        );
        let name = ui.add(
            egui::TextEdit::singleline(&mut page.add_name)
                .hint_text("name (optional)")
                .desired_width(140.0),
        );
        page.add_port.retain(|c| c.is_ascii_digit());
        page.add_port.truncate(5);
        let parsed = page.add_port.parse::<u16>().ok().filter(|&p| p != 0);
        let entered = (port.lost_focus() || name.lost_focus())
            && ui.input(|i| i.key_pressed(egui::Key::Enter));
        let add = ui
            .add_enabled(parsed.is_some(), egui::Button::new("Watch"))
            .on_hover_text("Keep an eye on this port");
        if let Some(port) = parsed
            && (add.clicked() || entered)
        {
            out.push(UiCmd::WatchPort(PortWatch {
                port,
                name: page.add_name.trim().to_string(),
            }));
            page.add_port.clear();
            page.add_name.clear();
        }
    });
}

pub(super) fn development_tab(
    ui: &mut Ui,
    page: &mut NetworkPage,
    snap: &Snapshot,
    activity: &NetActivity,
    watched: &[PortWatch],
    out: &mut Vec<UiCmd>,
) {
    let all = dev_ports(snap, activity, watched);
    watched_strip(ui, page, &all, watched, out);
    ui.add_space(4.0);
    let view = &mut page.development;
    let addresses = |r: &DevPort| {
        r.addresses
            .iter()
            .map(|a| a.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    };
    let total = all.len();
    let mut rows: Vec<DevPort> = all
        .into_iter()
        .filter(|r| {
            view.matches(&[
                &r.port.to_string(),
                &r.tool,
                &r.program,
                &r.pid.to_string(),
                &addresses(r),
            ])
        })
        .collect();
    rows.sort_by_key(|r| (r.port, r.pid));
    view.order(&mut rows, |a, b, column| match column {
        0 => a.port.cmp(&b.port),
        1 => text_cmp(&a.tool, &b.tool),
        2 => text_cmp(&a.program, &b.program),
        3 => a.pid.cmp(&b.pid),
        4 => a.addresses.first().cmp(&b.addresses.first()),
        _ => a.open_to.0.cmp(b.open_to.0),
    });
    ui.horizontal(|ui| {
        if view.filter_box(ui, "Filter: port, tool, program", rows.len(), total) {
            ui.ctx().request_repaint();
        }
        ui.label(
            RichText::new(
                "Dev servers, databases and debuggers, told by the program and the port its tool \
                 uses; and the ports you watch",
            )
            .small()
            .color(theme::TEXT_WEAK),
        );
    });
    if rows.is_empty() {
        ui.add_space(20.0);
        ui.label(
            RichText::new(if total == 0 {
                "No development server is listening, and no port is watched."
            } else {
                "No port matches the filter."
            })
            .color(theme::TEXT_WEAK),
        );
        return;
    }
    egui::CentralPanel::no_frame().show(ui, |ui| {
        // The last, untitled, has the Free… button.
        const TITLES: [(&str, bool); 7] = [
            ("Port", true),
            ("Tool", false),
            ("Program", false),
            ("PID", true),
            ("Address", false),
            ("Open to", false),
            ("", false),
        ];
        const COLUMNS: [Spec; 7] = [
            Spec::new(70.0),
            Spec::new(190.0).clip(),
            Spec::new(200.0).clip(),
            Spec::new(64.0),
            Spec::new(220.0).clip(),
            Spec::new(130.0).clip(),
            Spec::new(80.0),
        ];
        Columns::load(ui.ctx(), "net-development", &COLUMNS).show(ui, |table, columns| {
            table
                .striped(true)
                .resizable(true)
                .auto_shrink([false, false])
                .sense(Sense::click())
                .cell_layout(Layout::left_to_right(Align::Center))
                .header(20.0, |mut h| {
                    columns.titles(&mut h, |ui, c| {
                        let (title, numeric) = TITLES[c];
                        if !title.is_empty() {
                            view.header(ui, c, title, numeric);
                        }
                    });
                })
                .body(|body| {
                    columns.keep(body.widths());
                    body.rows(20.0, rows.len(), |mut row| {
                        let r = &rows[row.index()];
                        for &c in columns.order() {
                            row.col(|ui| match c {
                                0 => right(ui, widgets::strong(r.port.to_string(), theme::BODY)),
                                1 => {
                                    let mut text = RichText::new(&r.tool);
                                    if r.watched {
                                        text = text.color(theme::TEXT);
                                    } else {
                                        text = text.color(theme::TEXT_WEAK);
                                    }
                                    ui.add(Label::new(text).truncate());
                                }
                                2 => {
                                    ui.add(Label::new(&*r.program).truncate());
                                }
                                3 => {
                                    if r.pid != 0 {
                                        right(
                                            ui,
                                            RichText::new(r.pid.to_string())
                                                .color(theme::TEXT_WEAK),
                                        );
                                    }
                                }
                                4 => {
                                    ui.add(
                                        Label::new(
                                            RichText::new(addresses(r))
                                                .monospace()
                                                .color(theme::TEXT_WEAK),
                                        )
                                        .truncate(),
                                    );
                                }
                                5 => {
                                    ui.label(RichText::new(r.open_to.0).color(r.open_to.1));
                                }
                                _ => {
                                    if r.pid != 0
                                        && ui
                                            .small_button("Free\u{2026}")
                                            .on_hover_text(format!(
                                                "End {} so the port is free",
                                                r.program
                                            ))
                                            .clicked()
                                    {
                                        out.push(UiCmd::FreePort {
                                            port: r.port,
                                            pid: r.pid,
                                        });
                                    }
                                }
                            });
                        }
                        let what = (r.port, r.pid);
                        widgets::row_menu(&row.response(), "net-development", what, |ui| {
                            if let Some(p) = process(snap, r.pid) {
                                item(ui, out, "Go to process", "", UiCmd::GoTo(p.key));
                                item(
                                    ui,
                                    out,
                                    &format!("Free port {}\u{2026}", r.port),
                                    "",
                                    UiCmd::FreePort {
                                        port: r.port,
                                        pid: r.pid,
                                    },
                                );
                                ui.separator();
                            }
                            if r.watched {
                                item(
                                    ui,
                                    out,
                                    "Stop watching this port",
                                    "",
                                    UiCmd::UnwatchPort(r.port),
                                );
                            } else {
                                item(
                                    ui,
                                    out,
                                    "Watch this port",
                                    "",
                                    UiCmd::WatchPort(PortWatch {
                                        port: r.port,
                                        name: String::new(),
                                    }),
                                );
                            }
                            item(ui, out, "Copy port", "", UiCmd::Copy(r.port.to_string()));
                        });
                    });
                })
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use anabolic_core::history::History;

    #[test]
    fn one_row_per_port_and_program_with_the_watched_free_ones() {
        let mut snap = Snapshot::empty(1, Arc::new(History::new(4, 1, &[])));
        let mut node = ProcessInfo::blank(
            ProcKey {
                pid: 40,
                create_time: 1,
            },
            1,
            "node.exe".into(),
        );
        node.state = ProcState::Running;
        snap.processes = vec![node];
        let listen = |pid, port, local: &str| Listener {
            pid,
            proto: Proto::Tcp,
            local: local.parse().unwrap(),
            port,
        };
        let activity = NetActivity {
            connections: Vec::new(),
            listeners: vec![
                listen(40, 5173, "127.0.0.1"),
                listen(40, 5173, "::1"),
                // Some program's port: not a tool's, and not watched.
                listen(44, 50123, "127.0.0.1"),
                listen(40, 3000, "0.0.0.0"),
            ],
            error: None,
        };
        let watched = [
            PortWatch {
                port: 3000,
                name: "my API".into(),
            },
            PortWatch {
                port: 8080,
                name: String::new(),
            },
        ];
        let mut rows = dev_ports(&snap, &activity, &watched);
        rows.sort_by_key(|r| r.port);
        let seen: Vec<(u16, &str, usize)> = rows
            .iter()
            .map(|r| (r.port, r.tool.as_str(), r.addresses.len()))
            .collect();
        assert_eq!(
            seen,
            [(3000, "my API", 1), (5173, "Vite", 2), (8080, "watched", 0)]
        );
        assert_eq!(rows[2].open_to.0, "free");
        assert!(rows[0].watched && !rows[1].watched);
    }
}

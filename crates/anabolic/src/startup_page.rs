//! The Startup page: what starts with Windows (the Run keys, the Startup
//! folders, tasks at sign-in or boot), whether it is on, and whether it is
//! running now and what it costs. On and off as Task Manager does it.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::sync::Arc;

use anabolic_core::startup::{StartupItem, StartupList};
use anabolic_core::sysinfo::local_time;
use anabolic_core::{ProcState, ProcessDetails, ProcessInfo, Snapshot};
use eframe::egui::{self, Align, Label, Layout, RichText, Sense, Ui};
use egui_extras::{Column, TableBuilder};

use crate::proc_table::{UiCmd, item};
use crate::table::{TableView, text_cmp};
use crate::{fmt, panes, theme, widgets};

#[derive(Default)]
pub struct StartupPage {
    pub list: Option<Arc<StartupList>>,
    pub view: TableView,
    /// Also list Windows' own tasks.
    pub windows: bool,
    /// By `location` and `name`, which together name an entry.
    pub selected: Option<(String, String)>,
    /// A change was sent and has not answered yet.
    pub busy: bool,
}

/// What a person calls the entry: the program's own description, unless
/// it only repeats the file's name (Discord's "Update"), else the entry's.
fn title(i: &StartupItem) -> &str {
    let stem = i
        .program
        .as_deref()
        .and_then(|p| std::path::Path::new(p).file_stem())
        .and_then(|s| s.to_str())
        .unwrap_or("");
    i.description
        .as_deref()
        .filter(|d| !d.is_empty() && !d.eq_ignore_ascii_case(stem))
        .unwrap_or(&i.name)
}

fn key(i: &StartupItem) -> (String, String) {
    (i.location.clone(), i.name.clone())
}

/// The processes running each program, by lower-case path.
fn running<'a>(
    snap: &'a Snapshot,
    details: &HashMap<anabolic_core::ProcKey, Arc<ProcessDetails>>,
) -> HashMap<String, Vec<&'a ProcessInfo>> {
    let mut map: HashMap<String, Vec<&ProcessInfo>> = HashMap::new();
    for p in &snap.processes {
        if p.state == ProcState::Exited {
            continue;
        }
        if let Some(path) = details.get(&p.key).and_then(|d| d.path.as_deref()) {
            map.entry(path.to_lowercase()).or_default().push(p);
        }
    }
    map
}

fn status(i: &StartupItem) -> (&'static str, egui::Color32) {
    if i.missing {
        ("Program missing", theme::ELEVATED_TEXT)
    } else if i.enabled {
        ("On", theme::TEXT)
    } else {
        ("Off", theme::TEXT_DIM)
    }
}

pub fn startup_menu(
    ui: &mut Ui,
    i: &StartupItem,
    procs: &[&ProcessInfo],
    busy: bool,
    out: &mut Vec<UiCmd>,
) {
    ui.add_enabled_ui(!busy, |ui| {
        let (text, on) = if i.enabled {
            ("Turn off", false)
        } else {
            ("Turn on", true)
        };
        item(
            ui,
            out,
            text,
            "",
            UiCmd::SetStartup(Box::new(i.clone()), on),
        );
    });
    ui.separator();
    if let Some(p) = procs.first() {
        item(ui, out, "Go to process", "", UiCmd::GoTo(p.key));
    }
    if let Some(program) = i.program.clone().filter(|_| !i.missing) {
        item(
            ui,
            out,
            "Open file location",
            "",
            UiCmd::OpenLocation(program.clone()),
        );
        item(ui, out, "Properties", "", UiCmd::Properties(program));
    }
    item(
        ui,
        out,
        "Search online",
        "",
        UiCmd::SearchOnline(title(i).to_string()),
    );
    ui.separator();
    item(ui, out, "Copy command", "", UiCmd::Copy(i.command.clone()));
}

fn pane(ui: &mut Ui, i: &StartupItem, procs: &[&ProcessInfo], busy: bool, out: &mut Vec<UiCmd>) {
    panes::title(ui, title(i).to_string(), i.publisher.as_deref());
    ui.horizontal(|ui| {
        ui.add_enabled_ui(!busy, |ui| {
            let (text, on) = if i.enabled {
                ("Turn off", false)
            } else {
                ("Turn on", true)
            };
            if ui.button(text).clicked() {
                out.push(UiCmd::SetStartup(Box::new(i.clone()), on));
            }
        });
        if busy {
            ui.spinner();
        }
    });
    if i.source.is_task() {
        panes::weak(
            ui,
            "A scheduled task: turning it off disables the task, as Task Scheduler would.",
        );
    } else {
        panes::weak(
            ui,
            "Turning it off leaves the entry in place, switched off the way Task Manager \
             does it; turning it on again restores it.",
        );
    }

    panes::section(ui, "Entry");
    panes::facts(ui, ("startup-facts", &i.location, &i.name), |ui| {
        let (text, color) = status(i);
        panes::fact(ui, "Status", RichText::new(text).color(color));
        if let Some(at) = i.disabled_at.and_then(local_time) {
            panes::fact(ui, "Turned off", at);
        }
        panes::fact(ui, "Kind", i.source.label());
        panes::fact(ui, "Name", i.name.clone());
        panes::fact(ui, "Where", RichText::new(&i.location).small());
        panes::fact(ui, "Runs", RichText::new(&i.command).small());
        if let Some(p) = &i.program {
            let text = if i.missing {
                RichText::new(format!("{p}  (not found)")).color(theme::ELEVATED_TEXT)
            } else {
                RichText::new(p).small()
            };
            panes::fact(ui, "Program", text);
        }
    });

    panes::section(ui, "Now");
    if procs.is_empty() {
        panes::weak(ui, "Not running.");
    }
    for p in procs {
        ui.horizontal(|ui| {
            if widgets::link(ui, &format!("{} ({})", p.name, p.pid)).clicked() {
                out.push(UiCmd::GoTo(p.key));
            }
            ui.label(
                RichText::new(format!(
                    "{} % CPU  ·  {}",
                    if p.cpu > 0.0 {
                        fmt::cpu(p.cpu)
                    } else {
                        "0".into()
                    },
                    fmt::bytes(p.private_bytes)
                ))
                .color(theme::TEXT_WEAK),
            );
        });
    }
}

pub fn show(
    ui: &mut Ui,
    page: &mut StartupPage,
    snap: &Snapshot,
    details: &HashMap<anabolic_core::ProcKey, Arc<ProcessDetails>>,
    elevated: bool,
    out: &mut Vec<UiCmd>,
) {
    let Some(list) = page.list.clone() else {
        ui.add_space(20.0);
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(RichText::new("Reading what starts with Windows…").color(theme::TEXT_WEAK));
        });
        return;
    };
    let running = running(snap, details);
    let procs = |i: &StartupItem| -> Vec<&ProcessInfo> {
        i.program
            .as_deref()
            .and_then(|p| running.get(&p.to_lowercase()))
            .cloned()
            .unwrap_or_default()
    };
    let shown_windows = page.windows;
    let view = &mut page.view;
    let mut rows: Vec<&StartupItem> = list
        .items
        .iter()
        .filter(|i| shown_windows || !i.windows)
        .filter(|i| {
            view.matches(&[
                title(i),
                &i.name,
                i.publisher.as_deref().unwrap_or(""),
                &i.command,
                i.source.label(),
            ])
        })
        .collect();
    let cpu = |i: &StartupItem| procs(i).iter().map(|p| p.cpu).sum::<f32>();
    let memory = |i: &StartupItem| procs(i).iter().map(|p| p.private_bytes).sum::<u64>();
    view.order(&mut rows, |a, b, column| match column {
        0 => text_cmp(title(a), title(b)),
        1 => text_cmp(
            a.publisher.as_deref().unwrap_or(""),
            b.publisher.as_deref().unwrap_or(""),
        ),
        2 => status(a).0.cmp(status(b).0),
        3 => a.source.label().cmp(b.source.label()),
        4 => cpu(a).partial_cmp(&cpu(b)).unwrap_or(Ordering::Equal),
        5 => memory(a).cmp(&memory(b)),
        _ => text_cmp(&a.command, &b.command),
    });
    let total = list
        .items
        .iter()
        .filter(|i| shown_windows || !i.windows)
        .count();
    let on = list
        .items
        .iter()
        .filter(|i| (shown_windows || !i.windows) && i.enabled)
        .count();

    ui.horizontal(|ui| {
        if view.filter_box(ui, "Filter: name, publisher, command", rows.len(), total) {
            ui.ctx().request_repaint();
        }
        ui.add_space(6.0);
        ui.checkbox(&mut page.windows, "Windows' own tasks")
            .on_hover_text(
                "Tasks under \\Microsoft\\ in Task Scheduler that run at sign-in or boot.",
            );
        ui.add_space(6.0);
        ui.label(
            RichText::new(format!(
                "{total} entries  ·  {on} on  ·  read in {}  ·  services that start \
                 automatically are on the Services page",
                fmt::millis(list.took)
            ))
            .small()
            .color(theme::TEXT_WEAK),
        );
        if !elevated {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(
                    RichText::new("Entries for all users need administrator")
                        .small()
                        .color(theme::TEXT_DIM),
                );
                ui.label(widgets::icon(theme::icon::LOCK, 11.0).color(theme::TEXT_DIM));
            });
        }
    });
    for e in &list.errors {
        ui.label(
            RichText::new(format!("Could not read {e}"))
                .small()
                .color(theme::ELEVATED_TEXT),
        );
    }
    ui.add_space(2.0);

    let selected = page.selected.clone();
    if let Some(i) = selected
        .as_ref()
        .and_then(|k| list.items.iter().find(|i| &key(i) == k))
    {
        let procs = procs(i);
        let busy = page.busy;
        panes::side(ui, ("startup-pane", &i.location, &i.name), |ui| {
            pane(ui, i, &procs, busy, out)
        });
    }
    let busy = page.busy;
    egui::CentralPanel::no_frame().show(ui, |ui| {
        TableBuilder::new(ui)
            .id_salt("startup")
            .striped(true)
            .resizable(true)
            .auto_shrink([false, false])
            .sense(Sense::click())
            .cell_layout(Layout::left_to_right(Align::Center))
            .column(Column::initial(260.0).clip(true))
            .column(Column::initial(170.0).clip(true))
            .column(Column::initial(110.0))
            .column(Column::initial(170.0).clip(true))
            .column(Column::initial(60.0))
            .column(Column::initial(80.0))
            .column(Column::remainder().clip(true))
            .header(20.0, |mut h| {
                for (n, (t, numeric)) in [
                    ("Name", false),
                    ("Publisher", false),
                    ("Status", false),
                    ("Kind", false),
                    ("CPU %", true),
                    ("Memory", true),
                    ("Command", false),
                ]
                .into_iter()
                .enumerate()
                {
                    h.col(|ui| {
                        view.header(ui, n, t, numeric);
                    });
                }
            })
            .body(|body| {
                body.rows(18.0, rows.len(), |mut row| {
                    let i = rows[row.index()];
                    let procs = procs(i);
                    row.set_selected(selected.as_ref() == Some(&key(i)));
                    row.col(|ui| {
                        let mut text = RichText::new(title(i));
                        if !i.enabled {
                            text = text.color(theme::TEXT_WEAK);
                        }
                        ui.add(Label::new(text).truncate());
                    });
                    row.col(|ui| {
                        ui.add(
                            Label::new(
                                RichText::new(i.publisher.as_deref().unwrap_or(""))
                                    .color(theme::TEXT_WEAK),
                            )
                            .truncate(),
                        );
                    });
                    row.col(|ui| {
                        let (text, color) = status(i);
                        ui.label(RichText::new(text).color(color));
                    });
                    row.col(|ui| {
                        ui.add(
                            Label::new(RichText::new(i.source.label()).color(theme::TEXT_WEAK))
                                .truncate(),
                        );
                    });
                    row.col(|ui| {
                        if !procs.is_empty() {
                            right(ui, fmt::cpu(procs.iter().map(|p| p.cpu).sum()));
                        }
                    });
                    row.col(|ui| {
                        if !procs.is_empty() {
                            right(ui, fmt::bytes(procs.iter().map(|p| p.private_bytes).sum()));
                        }
                    });
                    row.col(|ui| {
                        ui.add(
                            Label::new(RichText::new(&i.command).small().color(theme::TEXT_DIM))
                                .truncate(),
                        );
                    });
                    let response = row.response();
                    if response.clicked() {
                        page.selected = if selected.as_ref() == Some(&key(i)) {
                            None
                        } else {
                            Some(key(i))
                        };
                    }
                    response.context_menu(|ui| startup_menu(ui, i, &procs, busy, out));
                });
            });
    });
}

fn right(ui: &mut Ui, text: String) {
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        ui.add(Label::new(text).selectable(false));
    });
}

// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! The Handles tab: what a process has open, by type and name, with the
//! side pane for one handle. On Linux these are its open files (file
//! descriptors), numbered from 0 and named as /proc gives them.

use super::*;

/// A handle's number as the system writes it: hex on Windows, a plain file
/// descriptor number on Linux.
pub(crate) fn value_text(value: u64) -> String {
    if cfg!(windows) {
        format!("0x{value:X}")
    } else {
        value.to_string()
    }
}

/// The number as the filter matches it: without the "0x", which every
/// handle on Windows has (a filter of "x" would keep them all).
pub(crate) fn value_filter_text(value: u64) -> String {
    if cfg!(windows) {
        format!("{value:X}")
    } else {
        value.to_string()
    }
}

/// What a handle may do: Windows' access mask, or on Linux whether the
/// file is open for reading, writing or both.
pub(crate) fn access_text(access: u32) -> String {
    if cfg!(windows) {
        return format!("0x{access:08X}");
    }
    match access {
        1 => "read".into(),
        2 => "write".into(),
        3 => "read, write".into(),
        _ => String::new(),
    }
}

/// Windows' words, or Linux's.
#[cfg(windows)]
mod words {
    pub const READING: &str = "Reading the handle table…";
    pub const COUNTED: &str = "handles";
    pub const SOURCE: &str = "read from the kernel handle table, the process was not opened";
    pub const FILTER: &str = "Filter: handle, type, name";
    pub const NUMBER: &str = "Handle";
    pub const EVERY: &str = "Every handle";
}

#[cfg(target_os = "linux")]
mod words {
    pub const READING: &str = "Reading its open files…";
    pub const COUNTED: &str = "open files";
    pub const SOURCE: &str = "read from /proc, the process was not stopped";
    pub const FILTER: &str = "Filter: number, type, name";
    pub const NUMBER: &str = "File";
    pub const EVERY: &str = "Every open file";
}

pub(super) fn handles_tab(
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
        // Linux names every open file as it lists it: nothing to resolve.
        if cfg!(windows)
            && ui
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
            ui.label(RichText::new(words::READING).color(theme::TEXT_WEAK));
        } else if let Some(r) = &current {
            ui.label(
                RichText::new(format!(
                    "{} {}  ·  {} system-wide  ·  {}  ·  {}",
                    fmt::count(r.entries.len() as u64),
                    words::COUNTED,
                    fmt::count(r.system_total as u64),
                    fmt::millis(r.took),
                    words::SOURCE,
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
            changed |= view.table.filter_box(ui, words::FILTER, shown, total);
            if report.resolved > 0 {
                ui.add_space(8.0);
                ui.selectable_value(&mut view.by_name, true, "Grouped by name");
                ui.selectable_value(&mut view.by_name, false, words::EVERY);
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
            const TITLES: [(&str, bool); 3] =
                [("Count", true), ("Type", false), ("Name / target", false)];
            const COLUMNS: [Spec; 3] = [Spec::new(80.0), Spec::new(120.0), Spec::new(400.0).clip()];
            let groups = &view.group_rows;
            Columns::load(ui.ctx(), "handle-names", &COLUMNS).show(ui, |table, columns| {
                table
                    .striped(true)
                    .resizable(true)
                    .auto_shrink([false, false])
                    .sense(Sense::click())
                    .cell_layout(Layout::left_to_right(Align::Center))
                    .header(20.0, |mut h| {
                        columns.titles(&mut h, |ui, c| {
                            let (title, numeric) = TITLES[c];
                            changed |= view.groups.header(ui, c, title, numeric);
                        });
                    })
                    .body(|body| {
                        columns.keep(body.widths());
                        body.rows(18.0, groups.len(), |mut row| {
                            let (t, name, count) =
                                &report.name_groups[groups[row.index()] as usize];
                            for &c in columns.order() {
                                row.col(|ui| match c {
                                    0 => right(ui, fmt::count(*count as u64)),
                                    1 => {
                                        ui.label(&*report.type_name(*t));
                                    }
                                    _ => {
                                        ui.add(Label::new(&**name).truncate());
                                    }
                                });
                            }
                            row.response().context_menu(|ui| {
                                crate::panes::group_menu(ui, &report.type_name(*t), name, out);
                            });
                        });
                    })
            });
        } else {
            const TITLES: [&str; 4] = [words::NUMBER, "Type", "Access", "Name / target"];
            const COLUMNS: [Spec; 4] = [
                Spec::new(90.0),
                Spec::new(120.0),
                Spec::new(100.0),
                Spec::new(400.0).clip(),
            ];
            let rows = &view.rows;
            Columns::load(ui.ctx(), "handle-list", &COLUMNS).show(ui, |table, columns| {
                table
                    .striped(true)
                    .resizable(true)
                    .auto_shrink([false, false])
                    .sense(Sense::click())
                    .cell_layout(Layout::left_to_right(Align::Center))
                    .header(20.0, |mut h| {
                        columns.titles(&mut h, |ui, c| {
                            changed |= view.table.header(ui, c, TITLES[c], false);
                        });
                    })
                    .body(|body| {
                        columns.keep(body.widths());
                        body.rows(18.0, rows.len(), |mut row| {
                            let i = rows[row.index()] as usize;
                            let e = &report.entries[i];
                            row.set_selected(selected == Some(e.value));
                            for &c in columns.order() {
                                row.col(|ui| match c {
                                    0 => {
                                        ui.label(RichText::new(value_text(e.value)).monospace());
                                    }
                                    1 => {
                                        ui.label(&*report.type_name(e.type_index));
                                    }
                                    2 => {
                                        ui.label(
                                            RichText::new(access_text(e.access))
                                                .monospace()
                                                .color(theme::TEXT_WEAK),
                                        );
                                    }
                                    _ => {
                                        if let Some(Some(name)) = report.names.get(i) {
                                            ui.add(Label::new(&**name).truncate());
                                        }
                                    }
                                });
                            }
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
                    })
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

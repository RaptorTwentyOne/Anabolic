// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! The Startup page's Everything: what starts by itself from every place
//! Autoruns looks, each with the file it runs and who signed it; Windows'
//! own hidden unless asked, and what deserves a look (no signature, a
//! missing file, a setting off Windows' default) first. Read in the
//! background when shown, and again on request.

use std::sync::Arc;

use anabolic_core::autoruns::{Autorun, Autoruns, Category};
use crossbeam_channel::{Receiver, bounded};
use eframe::egui::{self, Align, Label, Layout, RichText, Sense, Ui};

use crate::proc_table::UiCmd;
use crate::table::{Columns, Spec};
use crate::{panes, theme, widgets};

#[derive(Default)]
pub struct EverythingView {
    read: Option<Arc<Autoruns>>,
    reading: Option<Receiver<Autoruns>>,
    /// Windows' own too.
    pub system: bool,
    pub category: Option<Category>,
    pub filter: String,
    /// By location and name.
    selected: Option<(String, String)>,
}

impl EverythingView {
    fn start(&mut self, ctx: &egui::Context) {
        let (tx, rx) = bounded(1);
        let ctx = ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("autoruns".into())
            .spawn(move || {
                let _ = tx.send(anabolic_core::autoruns::read());
                ctx.request_repaint();
            });
        if spawned.is_ok() {
            self.reading = Some(rx);
        }
    }
}

fn matches(a: &Autorun, filter: &str) -> bool {
    if filter.is_empty() {
        return true;
    }
    let f = filter.to_lowercase();
    [
        Some(a.name.as_str()),
        Some(a.command.as_str()),
        Some(a.location.as_str()),
        a.path.as_deref(),
        a.description.as_deref(),
        a.company.as_deref(),
    ]
    .into_iter()
    .flatten()
    .any(|s| s.to_lowercase().contains(&f))
        || a.signer().to_lowercase().contains(&f)
}

fn color(a: &Autorun) -> egui::Color32 {
    if a.missing || a.note.is_some() {
        theme::ELEVATED_TEXT
    } else if a.suspicious() {
        theme::BAD
    } else if !a.enabled {
        theme::TEXT_WEAK
    } else {
        theme::TEXT
    }
}

pub fn show(ui: &mut Ui, view: &mut EverythingView, out: &mut Vec<UiCmd>) {
    if let Some(rx) = &view.reading
        && let Ok(read) = rx.try_recv()
    {
        view.read = Some(Arc::new(read));
        view.reading = None;
    }
    if view.read.is_none() && view.reading.is_none() {
        view.start(ui.ctx());
    }
    let Some(all) = view.read.clone() else {
        ui.add_space(20.0);
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(
                RichText::new("Reading every place things start from, and their signatures…")
                    .color(theme::TEXT_WEAK),
            );
        });
        return;
    };
    let others = all.entries.iter().filter(|a| !a.system).count();
    let look = all
        .entries
        .iter()
        .filter(|a| !a.system && a.suspicious())
        .count();
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(format!(
                "{} entries, {others} not Windows' own, {look} worth a look  ·  read in {}",
                all.entries.len(),
                crate::fmt::millis(all.took)
            ))
            .small()
            .color(theme::TEXT_WEAK),
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let reading = view.reading.is_some();
            if ui
                .add_enabled(!reading, egui::Button::new("Read again"))
                .clicked()
            {
                view.start(ui.ctx());
            }
            if reading {
                ui.spinner();
            }
        });
    });
    for e in &all.errors {
        ui.label(RichText::new(e).small().color(theme::ELEVATED_TEXT));
    }
    ui.horizontal_wrapped(|ui| {
        if ui
            .selectable_label(view.category.is_none(), "All")
            .clicked()
        {
            view.category = None;
        }
        let mut categories: Vec<Category> = all.entries.iter().map(|a| a.category).collect();
        categories.sort();
        categories.dedup();
        for c in categories {
            let n = all
                .entries
                .iter()
                .filter(|a| a.category == c && (view.system || !a.system))
                .count();
            if ui
                .selectable_label(view.category == Some(c), format!("{} {n}", c.label()))
                .clicked()
            {
                view.category = if view.category == Some(c) {
                    None
                } else {
                    Some(c)
                };
            }
        }
    });
    ui.horizontal(|ui| {
        ui.checkbox(&mut view.system, "Windows' own too")
            .on_hover_text(if cfg!(windows) {
                "Entries whose file Microsoft signed"
            } else {
                "Entries the system's packages put in place"
            });
        ui.add_space(8.0);
        widgets::clearable(ui, &mut view.filter, |t| {
            egui::TextEdit::singleline(t)
                .hint_text("Filter: name, file, signer, place")
                .desired_width(260.0)
        });
    });
    ui.add_space(4.0);

    let mut shown: Vec<&Autorun> = all
        .entries
        .iter()
        .filter(|a| view.system || !a.system)
        .filter(|a| view.category.is_none_or(|c| a.category == c))
        .filter(|a| matches(a, view.filter.trim()))
        .collect();
    // What deserves a look first, then in their places' order.
    shown.sort_by_key(|a| (!a.suspicious(), a.category, a.name.to_lowercase()));

    if let Some(a) = view.selected.as_ref().and_then(|(l, n)| {
        all.entries
            .iter()
            .find(|a| &a.location == l && &a.name == n)
    }) {
        let mut close = false;
        panes::side(ui, ("autorun-pane", &a.location, &a.name), |ui| {
            close = pane(ui, a, out)
        });
        if close {
            view.selected = None;
        }
    }
    egui::CentralPanel::no_frame().show(ui, |ui| table(ui, &shown, &mut view.selected));
}

const TITLES: [(&str, bool); 6] = [
    ("Entry", false),
    ("Kind", false),
    ("Signed by", false),
    ("File", false),
    ("What it is", false),
    ("Where", false),
];
const COLUMNS: [Spec; 6] = [
    Spec::new(220.0).clip(),
    Spec::new(120.0).clip(),
    Spec::new(190.0).clip(),
    Spec::new(300.0).clip(),
    Spec::new(200.0).clip(),
    Spec::new(320.0).clip(),
];

fn table(ui: &mut Ui, shown: &[&Autorun], selected: &mut Option<(String, String)>) {
    let mut pick = None;
    Columns::load(ui.ctx(), "autoruns", &COLUMNS).show(ui, |table, columns| {
        table
            .striped(true)
            .resizable(true)
            .auto_shrink([false, false])
            .sense(Sense::click())
            .cell_layout(Layout::left_to_right(Align::Center))
            .header(20.0, |mut h| {
                columns.titles(&mut h, |ui, c| {
                    let (title, numeric) = TITLES[c];
                    widgets::column_header(ui, title, numeric);
                });
            })
            .body(|body| {
                columns.keep(body.widths());
                body.rows(widgets::ROW, shown.len(), |mut row| {
                    let a = shown[row.index()];
                    let is = selected
                        .as_ref()
                        .is_some_and(|(l, n)| *l == a.location && *n == a.name);
                    row.set_selected(is);
                    for &c in columns.order() {
                        row.col(|ui| {
                            let text = match c {
                                0 => RichText::new(&a.name).color(color(a)),
                                1 => RichText::new(a.category.label()).color(theme::TEXT_WEAK),
                                2 => RichText::new(a.signer()).color(if a.suspicious() {
                                    color(a)
                                } else {
                                    theme::TEXT_WEAK
                                }),
                                3 => RichText::new(a.path.as_deref().unwrap_or(&a.command)),
                                4 => RichText::new(
                                    a.note.as_deref().or(a.description.as_deref()).unwrap_or(""),
                                )
                                .color(if a.note.is_some() {
                                    theme::ELEVATED_TEXT
                                } else {
                                    theme::TEXT_WEAK
                                }),
                                _ => RichText::new(&a.location).color(theme::TEXT_WEAK),
                            };
                            ui.add(Label::new(text).truncate());
                        });
                    }
                    if row.response().clicked() {
                        pick = Some((a.location.clone(), a.name.clone()));
                    }
                });
            })
    });
    if let Some(p) = pick {
        *selected = if selected.as_ref() == Some(&p) {
            None
        } else {
            Some(p)
        };
    }
}

/// One entry in full; true when it is closed.
fn pane(ui: &mut Ui, a: &Autorun, out: &mut Vec<UiCmd>) -> bool {
    let mut close = false;
    ui.horizontal(|ui| {
        ui.add(Label::new(widgets::strong(&a.name, theme::SUBTITLE)).wrap());
        ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
            close = ui
                .small_button(widgets::icon(theme::icon::CLOSE, 10.0))
                .on_hover_text("Close")
                .clicked();
        });
    });
    ui.label(RichText::new(a.category.label()).color(theme::TEXT_WEAK));
    if let Some(note) = &a.note {
        ui.label(RichText::new(note).color(theme::ELEVATED_TEXT));
    }
    ui.add_space(6.0);
    panes::facts(ui, ("autorun-facts", &a.location, &a.name), |ui| {
        panes::fact(ui, "Signed by", a.signer());
        if let Some(d) = &a.description {
            panes::fact(ui, "What it is", d.clone());
        }
        if let Some(c) = &a.company {
            panes::fact(ui, "Company", c.clone());
        }
        if let Some(p) = &a.path {
            panes::fact(ui, "File", p.clone());
        }
        panes::fact(ui, "As written", a.command.clone());
        panes::fact(ui, "Set in", a.location.clone());
        if !a.enabled {
            panes::fact(ui, "State", "Turned off".to_string());
        }
    });
    ui.add_space(10.0);
    ui.horizontal_wrapped(|ui| {
        if let Some(p) = a.path.as_ref().filter(|_| !a.missing)
            && ui.button("Open the file's location").clicked()
        {
            out.push(UiCmd::OpenLocation(p.clone()));
        }
        if ui.button("Copy").clicked() {
            out.push(UiCmd::Copy(format!(
                "{}\n{}\n{}\n{}",
                a.name,
                a.path.as_deref().unwrap_or(&a.command),
                a.signer(),
                a.location
            )));
        }
        if ui.button("Search online").clicked() {
            let file = a
                .path
                .as_deref()
                .and_then(|p| p.rsplit(['\\', '/']).next())
                .unwrap_or(&a.name);
            out.push(UiCmd::SearchOnline(file.to_string()));
        }
    });
    if a.category == Category::Services || a.category == Category::Drivers {
        ui.add_space(6.0);
        panes::weak(
            ui,
            "The Services page starts, stops and sets its start type.",
        );
    }
    close
}

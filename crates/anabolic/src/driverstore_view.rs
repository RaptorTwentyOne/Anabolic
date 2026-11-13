// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Performance > Devices > Driver packages: what the driver store keeps,
//! in the manner of DriverStore Explorer, the old versions no device uses
//! first, removed after a question.

use std::sync::Arc;

use anabolic_core::driverstore::{Package, Store};
use crossbeam_channel::{Receiver, bounded};
use eframe::egui::{self, Align, Label, Layout, RichText, Sense, Ui};

use crate::proc_table::UiCmd;
use crate::table::{Columns, Spec};
use crate::{theme, widgets};

#[derive(Default)]
pub struct DriverStoreView {
    read: Option<Result<Arc<Store>, String>>,
    reading: Option<Receiver<Result<Store, String>>>,
    pub only_old: bool,
    /// Read again then: a removal is on its way.
    again_at: Option<std::time::Instant>,
}

impl DriverStoreView {
    fn start(&mut self, ctx: &egui::Context) {
        let (tx, rx) = bounded(1);
        let ctx = ctx.clone();
        if std::thread::Builder::new()
            .name("driver-store".into())
            .spawn(move || {
                let _ = tx.send(anabolic_core::driverstore::read());
                ctx.request_repaint();
            })
            .is_ok()
        {
            self.reading = Some(rx);
        }
    }

    /// Reads again in a moment: packages are being removed.
    pub fn changed(&mut self) {
        self.again_at = Some(std::time::Instant::now() + std::time::Duration::from_secs(3));
    }

    /// Reads again now.
    pub fn refresh(&mut self, ctx: &egui::Context) {
        if self.reading.is_none() {
            self.start(ctx);
        }
    }
}

fn state(p: &Package) -> (String, egui::Color32) {
    if p.removable() {
        (
            "Old version, no device uses it".into(),
            theme::ELEVATED_TEXT,
        )
    } else if p.devices > 0 {
        (
            format!(
                "{} device{}{}",
                p.devices,
                if p.devices == 1 { "" } else { "s" },
                if p.connected < p.devices {
                    format!(", {} connected", p.connected)
                } else {
                    String::new()
                }
            ),
            theme::TEXT,
        )
    } else {
        ("No device; the newest of its name".into(), theme::TEXT_WEAK)
    }
}

const TITLES: [&str; 7] = [
    "Package", "Maker", "Kind", "Version", "Date", "Size", "In use",
];
const COLUMNS: [Spec; 7] = [
    Spec::new(230.0).clip(),
    Spec::new(170.0).clip(),
    Spec::new(120.0).clip(),
    Spec::new(120.0).clip(),
    Spec::new(85.0),
    Spec::new(80.0),
    Spec::new(230.0).clip(),
];

pub fn show(ui: &mut Ui, view: &mut DriverStoreView, out: &mut Vec<UiCmd>) {
    if let Some(rx) = &view.reading
        && let Ok(r) = rx.try_recv()
    {
        view.read = Some(r.map(Arc::new));
        view.reading = None;
    }
    if view.read.is_none() && view.reading.is_none() {
        view.start(ui.ctx());
    }
    if let Some(at) = view.again_at {
        if std::time::Instant::now() >= at {
            view.again_at = None;
            view.refresh(ui.ctx());
        } else {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(500));
        }
    }
    let store = match &view.read {
        None => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new("Reading the driver store…").color(theme::TEXT_WEAK));
            });
            return;
        }
        Some(Err(e)) => {
            ui.label(RichText::new(e).color(theme::BAD));
            return;
        }
        Some(Ok(s)) => s.clone(),
    };
    let old: Vec<&Package> = store.packages.iter().filter(|p| p.removable()).collect();
    let total: u64 = store.packages.iter().map(|p| p.size).sum();
    ui.add(
        Label::new(
            RichText::new(format!(
                "The third-party driver packages Windows keeps, in the manner of DriverStore \
                 Explorer: {} packages, {} in all. Graphics drivers leave a copy of every \
                 version installed; the old ones no device uses can go, and Windows refuses to \
                 remove one a device uses.",
                store.packages.len(),
                crate::fmt::bytes(total)
            ))
            .size(theme::META)
            .color(theme::TEXT_WEAK),
        )
        .wrap(),
    );
    ui.horizontal(|ui| {
        ui.checkbox(&mut view.only_old, "Only the old ones no device uses");
        ui.add_space(12.0);
        let size: u64 = old.iter().map(|p| p.size).sum();
        if ui
            .add_enabled(
                !old.is_empty(),
                egui::Button::new(format!(
                    "Remove {} old package{} ({})…",
                    old.len(),
                    if old.len() == 1 { "" } else { "s" },
                    crate::fmt::bytes(size)
                )),
            )
            .clicked()
        {
            out.push(UiCmd::ConfirmDriverPackages(
                old.iter()
                    .map(|p| (p.published.clone(), p.original.clone()))
                    .collect(),
            ));
        }
        if ui.button("Read again").clicked() {
            view.refresh(ui.ctx());
        }
        if view.reading.is_some() {
            ui.spinner();
        }
    });
    ui.add_space(4.0);
    let shown: Vec<&Package> = store
        .packages
        .iter()
        .filter(|p| !view.only_old || p.removable())
        .collect();
    let mut remove = None;
    Columns::load(ui.ctx(), "driver-store", &COLUMNS).show(ui, |table, columns| {
        table
            .striped(true)
            .resizable(true)
            .auto_shrink([false, false])
            .sense(Sense::click())
            .cell_layout(Layout::left_to_right(Align::Center))
            .header(20.0, |mut h| {
                columns.titles(&mut h, |ui, c| {
                    widgets::column_header(ui, TITLES[c], c == 5)
                });
            })
            .body(|body| {
                columns.keep(body.widths());
                body.rows(widgets::ROW, shown.len(), |mut row| {
                    let p = shown[row.index()];
                    let (words, color) = state(p);
                    for &c in columns.order() {
                        row.col(|ui| {
                            let text = match c {
                                0 => RichText::new(format!("{}  ·  {}", p.original, p.published)),
                                1 => RichText::new(&p.provider).color(theme::TEXT_WEAK),
                                2 => RichText::new(&p.class).color(theme::TEXT_WEAK),
                                3 => RichText::new(&p.version).color(theme::TEXT_WEAK),
                                4 => RichText::new(&p.sortable).color(theme::TEXT_WEAK),
                                5 => RichText::new(crate::fmt::bytes(p.size)),
                                _ => RichText::new(&words).color(color),
                            };
                            ui.add(Label::new(text).truncate());
                        });
                    }
                    if p.removable() {
                        row.response().context_menu(|ui| {
                            if ui.button("Remove this package…").clicked() {
                                remove = Some((p.published.clone(), p.original.clone()));
                                ui.close();
                            }
                        });
                    }
                });
            })
    });
    if let Some(p) = remove {
        out.push(UiCmd::ConfirmDriverPackages(vec![p]));
    }
}

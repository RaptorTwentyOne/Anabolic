// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Crashes > What changed: updates, drivers, programs and services day by
//! day, each day beside its crashes, in the manner of Reliability Monitor
//! and WinUpdatesView. Read in the background when first shown.

use std::sync::Arc;

use anabolic_core::changes::{Changes, Kind, days};
use anabolic_core::crashes::Crashes;
use crossbeam_channel::{Receiver, bounded};
use eframe::egui::{self, Color32, Label, RichText, Ui};

use crate::{theme, widgets};

/// Days drawn at once: the logs reach back months.
const DAYS_SHOWN: usize = 90;

#[derive(Default)]
pub struct ChangesView {
    read: Option<Result<Arc<Changes>, String>>,
    reading: Option<Receiver<Result<Changes, String>>>,
    /// Kinds left out.
    hidden: Vec<Kind>,
    filter: String,
}

impl ChangesView {
    fn start(&mut self, ctx: &egui::Context) {
        let (tx, rx) = bounded(1);
        let ctx = ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("changes".into())
            .spawn(move || {
                let _ = tx.send(anabolic_core::changes::read());
                ctx.request_repaint();
            });
        if spawned.is_ok() {
            self.reading = Some(rx);
        }
    }
}

fn color(k: Kind) -> Color32 {
    match k {
        Kind::Update => theme::CPU,
        Kind::UpdateFailed => theme::BAD,
        Kind::Driver => theme::GPU,
        Kind::Program => theme::DISK_READ,
        Kind::ProgramRemoved => theme::TEXT_WEAK,
        Kind::Service => theme::MEMORY,
    }
}

pub fn show(ui: &mut Ui, view: &mut ChangesView, crashes: Option<&Crashes>) {
    if let Some(rx) = &view.reading
        && let Ok(r) = rx.try_recv()
    {
        view.read = Some(r.map(Arc::new));
        view.reading = None;
    }
    if view.read.is_none() && view.reading.is_none() {
        view.start(ui.ctx());
    }
    let changes = match &view.read {
        None => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new("Reading what changed…").color(theme::TEXT_WEAK));
            });
            return;
        }
        Some(Err(e)) => {
            ui.label(RichText::new(e).color(theme::BAD));
            return;
        }
        Some(Ok(c)) => c.clone(),
    };
    ui.horizontal_wrapped(|ui| {
        for k in Kind::ALL {
            let n = changes.changes.iter().filter(|c| c.kind == k).count();
            let on = !view.hidden.contains(&k);
            if ui
                .selectable_label(
                    on,
                    RichText::new(format!("{} {n}", k.label())).color(color(k)),
                )
                .clicked()
            {
                if on {
                    view.hidden.push(k);
                } else {
                    view.hidden.retain(|h| *h != k);
                }
            }
        }
        ui.add_space(8.0);
        widgets::clearable(ui, &mut view.filter, |t| {
            egui::TextEdit::singleline(t)
                .hint_text("Filter: name, version, maker")
                .desired_width(220.0)
        });
        if ui.button("Read again").clicked() && view.reading.is_none() {
            view.start(ui.ctx());
        }
    });
    for note in &changes.notes {
        ui.label(RichText::new(note).small().color(theme::TEXT_WEAK));
    }
    ui.add_space(4.0);
    let f = view.filter.trim().to_lowercase();
    let kept: Vec<anabolic_core::changes::Change> = changes
        .changes
        .iter()
        .filter(|c| !view.hidden.contains(&c.kind))
        .filter(|c| {
            f.is_empty()
                || c.what.to_lowercase().contains(&f)
                || c.detail.to_lowercase().contains(&f)
        })
        .cloned()
        .collect();
    let by_day = days(&kept, crashes);
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for d in by_day.iter().take(DAYS_SHOWN) {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label(widgets::strong(&d.day, theme::BODY));
                    ui.label(
                        RichText::new(format!(
                            "{} change{}",
                            d.changes.len(),
                            if d.changes.len() == 1 { "" } else { "s" }
                        ))
                        .color(theme::TEXT_WEAK),
                    );
                    if d.crashes > 0 {
                        ui.label(
                            RichText::new(format!(
                                "·  {} crash{}",
                                d.crashes,
                                if d.crashes == 1 { "" } else { "es" }
                            ))
                            .color(theme::BAD),
                        );
                    }
                });
                // Fixed widths: in a grid a truncated label takes no room.
                let name_width = (ui.available_width() * 0.42).clamp(240.0, 520.0);
                let detail_width = (ui.available_width() - name_width - 190.0).max(160.0);
                let cell = |ui: &mut Ui, width: f32, text: RichText| -> egui::Response {
                    ui.allocate_ui_with_layout(
                        egui::vec2(width, 18.0),
                        egui::Layout::left_to_right(egui::Align::Center),
                        |ui| {
                            ui.set_width(width);
                            ui.add(Label::new(text).truncate())
                        },
                    )
                    .inner
                };
                egui::Grid::new(("change-day", &d.day))
                    .num_columns(4)
                    .spacing([14.0, 2.0])
                    .striped(true)
                    .show(ui, |ui| {
                        for &i in &d.changes {
                            let c = &kept[i];
                            let time = if c.day_only {
                                String::new()
                            } else {
                                anabolic_core::sysinfo::local_time(c.at)
                                    .map(|t| t.chars().skip(11).take(5).collect())
                                    .unwrap_or_default()
                            };
                            ui.label(RichText::new(time).color(theme::TEXT_DIM));
                            ui.label(RichText::new(c.kind.label()).color(color(c.kind)));
                            let what = if c.times > 1 {
                                format!("{} ({} times)", c.what, c.times)
                            } else {
                                c.what.clone()
                            };
                            cell(ui, name_width, RichText::new(&what)).on_hover_text(&what);
                            cell(
                                ui,
                                detail_width,
                                RichText::new(&c.detail).color(theme::TEXT_WEAK),
                            )
                            .on_hover_text(&c.detail);
                            ui.end_row();
                        }
                    });
            }
            if by_day.len() > DAYS_SHOWN {
                ui.label(
                    RichText::new(format!(
                        "{} earlier days; a filter narrows them down.",
                        by_day.len() - DAYS_SHOWN
                    ))
                    .color(theme::TEXT_DIM),
                );
            }
        });
}

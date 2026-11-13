// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Network > Data usage: what each program received and sent over the
//! last day, week, month or two, from Windows' own record (in the manner
//! of NetworkUsageView), from before Anabolic was installed too.

use std::sync::Arc;

use anabolic_core::datausage::Usage;
use crossbeam_channel::{Receiver, bounded};
use eframe::egui::{self, Label, RichText, Sense, Ui, vec2};

use crate::{fmt, theme, widgets};

const SPANS: [(u32, &str); 4] = [(1, "Day"), (7, "Week"), (30, "Month"), (60, "Two months")];

pub struct UsageView {
    days: u32,
    read: Option<(u32, Result<Arc<Usage>, String>)>,
    reading: Option<(u32, Receiver<Result<Usage, String>>)>,
    filter: String,
}

impl Default for UsageView {
    fn default() -> Self {
        UsageView {
            days: 30,
            read: None,
            reading: None,
            filter: String::new(),
        }
    }
}

impl UsageView {
    fn start(&mut self, ctx: &egui::Context) {
        let (tx, rx) = bounded(1);
        let (ctx, days) = (ctx.clone(), self.days);
        if std::thread::Builder::new()
            .name("data-usage".into())
            .spawn(move || {
                let _ = tx.send(anabolic_core::datausage::read(days));
                ctx.request_repaint();
            })
            .is_ok()
        {
            self.reading = Some((days, rx));
        }
    }
}

pub fn show(ui: &mut Ui, view: &mut UsageView) {
    if let Some((days, rx)) = &view.reading
        && let Ok(r) = rx.try_recv()
    {
        view.read = Some((*days, r.map(Arc::new)));
        view.reading = None;
    }
    let fresh = view.read.as_ref().is_some_and(|(d, _)| *d == view.days);
    let asked = view.reading.as_ref().is_some_and(|(d, _)| *d == view.days);
    if !fresh && !asked && view.reading.is_none() {
        view.start(ui.ctx());
    }
    ui.horizontal(|ui| {
        for (days, label) in SPANS {
            if ui.selectable_label(view.days == days, label).clicked() {
                view.days = days;
            }
        }
        ui.add_space(8.0);
        widgets::clearable(ui, &mut view.filter, |t| {
            egui::TextEdit::singleline(t)
                .hint_text("Filter programs")
                .desired_width(200.0)
        });
        if view.reading.is_some() {
            ui.spinner();
            ui.label(
                RichText::new("Windows takes a few seconds to add it up…").color(theme::TEXT_WEAK),
            );
        }
    });
    ui.add(
        Label::new(
            RichText::new(
                "From Windows' own record of what each program sent and received (the System \
                 Resource Usage Monitor, which Settings' Data usage page reads), kept for about \
                 60 days and from before Anabolic was installed too, as NetworkUsageView shows it.",
            )
            .size(theme::META)
            .color(theme::TEXT_WEAK),
        )
        .wrap(),
    );
    let Some((_, read)) = &view.read else {
        return;
    };
    let usage = match read {
        Ok(u) => u.clone(),
        Err(e) => {
            ui.label(RichText::new(e).color(theme::TEXT_WEAK));
            return;
        }
    };
    let (sent, received) = usage.total();
    ui.add_space(4.0);
    ui.label(
        RichText::new(format!(
            "The last {} day{}: {} received, {} sent, by {} programs on {} networks",
            usage.days,
            if usage.days == 1 { "" } else { "s" },
            fmt::bytes(received),
            fmt::bytes(sent),
            usage.apps.len(),
            usage.networks
        ))
        .color(theme::TEXT),
    );
    let f = view.filter.trim().to_lowercase();
    let top = usage.apps.first().map_or(1, |a| a.total()).max(1);
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            egui::Grid::new("data-usage")
                .num_columns(5)
                .spacing([18.0, 3.0])
                .striped(true)
                .show(ui, |ui| {
                    for t in ["Program", "Received", "Sent", "", "Where"] {
                        ui.label(RichText::new(t).strong().color(theme::TEXT_WEAK));
                    }
                    ui.end_row();
                    for a in usage
                        .apps
                        .iter()
                        .filter(|a| {
                            f.is_empty()
                                || a.name.to_lowercase().contains(&f)
                                || a.id.to_lowercase().contains(&f)
                        })
                        .take(300)
                    {
                        ui.label(&a.name);
                        ui.label(RichText::new(fmt::bytes(a.received)).color(theme::NET_RX));
                        ui.label(RichText::new(fmt::bytes(a.sent)).color(theme::NET_TX));
                        let (rect, _) = ui.allocate_exact_size(vec2(140.0, 8.0), Sense::hover());
                        let w = rect.width() * a.total() as f32 / top as f32;
                        let down = w * a.received as f32 / a.total().max(1) as f32;
                        ui.painter().rect_filled(rect, 2.0, theme::RAISED);
                        ui.painter().rect_filled(
                            egui::Rect::from_min_size(rect.min, vec2(down, rect.height())),
                            2.0,
                            theme::NET_RX,
                        );
                        ui.painter().rect_filled(
                            egui::Rect::from_min_size(
                                rect.min + vec2(down, 0.0),
                                vec2(w - down, rect.height()),
                            ),
                            2.0,
                            theme::NET_TX,
                        );
                        ui.add(Label::new(RichText::new(&a.id).color(theme::TEXT_DIM)).truncate());
                        ui.end_row();
                    }
                });
        });
}
